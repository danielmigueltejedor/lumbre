//! Linux adapter: procfs, sysfs, PSI and cgroup v2. No dependencies and no `unsafe`.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::time::Instant;

use crate::probe::{logical_cpus, Probe};
use crate::{parse, CgroupMemory, Cpu, Gpu, Hog, Memory, Power, Pressure, Snapshot};

/// `USER_HZ`: the unit of the times in `/proc/<pid>/stat`. It is 100 on every Linux ABI.
const USER_HZ: f32 = 100.0;
/// A process is reported as busy from half a core upwards.
const HOG_CORES: f32 = 0.5;

#[derive(Debug, Default)]
pub(crate) struct LinuxProbe {
    cpu: Option<(u64, u64)>,
    ticks: HashMap<u32, u64>,
    at: Option<Instant>,
}

fn read(path: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(path).ok()
}

pub(crate) fn process_rss(pid: u32) -> Option<u64> {
    parse::status_rss(&read(format!("/proc/{pid}/status"))?)
}

fn memory() -> Memory {
    let (total_bytes, available_bytes) =
        read("/proc/meminfo").and_then(|text| parse::meminfo(&text)).unwrap_or_default();
    let psi = read("/proc/pressure/memory").and_then(|text| parse::psi(&text));
    Memory { total_bytes, available_bytes, pressure: psi.map_or(Pressure::Unknown, |psi| psi.level()), psi }
}

fn power() -> Power {
    let Ok(entries) = fs::read_dir("/sys/class/power_supply") else { return Power::Unknown };
    let mut battery = None;
    let mut mains_online = None;
    for entry in entries.flatten() {
        let dir = entry.path();
        match read(dir.join("type")).as_deref().map(str::trim) {
            Some("Mains" | "USB") => {
                let online = read(dir.join("online")).is_some_and(|v| v.trim() == "1");
                mains_online = Some(mains_online.unwrap_or(false) || online);
            }
            Some("Battery") => {
                let discharging = read(dir.join("status")).is_some_and(|v| v.trim() == "Discharging");
                let percent = read(dir.join("capacity")).and_then(|v| v.trim().parse::<u8>().ok());
                battery = Some((discharging, percent));
            }
            _ => {}
        }
    }
    match (battery, mains_online) {
        (Some((true, percent)), _) => Power::Battery { percent },
        (Some(_), _) | (None, Some(true)) => Power::Ac,
        // A desktop has no power_supply entries at all, or only an offline USB port.
        (None, _) => Power::Ac,
    }
}

fn gpus() -> Vec<Gpu> {
    // amdgpu (and some other DRM drivers) publish VRAM counters in sysfs.
    let Ok(entries) = fs::read_dir("/sys/class/drm") else { return Vec::new() };
    let mut found = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with("card") || name.contains('-') {
            continue;
        }
        let device = entry.path().join("device");
        let value = |file: &str| read(device.join(file)).and_then(|v| v.trim().parse::<u64>().ok());
        if let (Some(vram_total_bytes), Some(vram_used_bytes)) =
            (value("mem_info_vram_total"), value("mem_info_vram_used"))
        {
            found.push(Gpu { name, vram_total_bytes, vram_used_bytes });
        }
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found
}

fn cgroup() -> Option<CgroupMemory> {
    let text = read("/proc/self/cgroup")?;
    let dir = Path::new("/sys/fs/cgroup").join(parse::cgroup_path(&text)?.trim_start_matches('/'));
    let current_bytes = read(dir.join("memory.current"))?.trim().parse().ok()?;
    let limit = |file: &str| read(dir.join(file)).and_then(|v| parse::cgroup_limit(&v));
    Some(CgroupMemory { current_bytes, max_bytes: limit("memory.max"), high_bytes: limit("memory.high") })
}

impl LinuxProbe {
    fn hogs(&mut self, elapsed: Option<f32>) -> Vec<Hog> {
        let mut now = HashMap::new();
        let mut hogs = Vec::new();
        let Ok(entries) = fs::read_dir("/proc") else { return hogs };
        for entry in entries.flatten() {
            let Some(pid) = entry.file_name().to_str().and_then(|name| name.parse::<u32>().ok()) else {
                continue;
            };
            let Some((name, ticks)) = read(entry.path().join("stat")).and_then(|text| parse::pid_stat(&text))
            else {
                continue;
            };
            if let (Some(before), Some(elapsed)) = (self.ticks.get(&pid), elapsed) {
                let cores = ticks.saturating_sub(*before) as f32 / USER_HZ / elapsed;
                if cores >= HOG_CORES {
                    hogs.push(Hog { pid, name, cores });
                }
            }
            now.insert(pid, ticks);
        }
        self.ticks = now;
        hogs.sort_by(|a, b| b.cores.total_cmp(&a.cores));
        hogs.truncate(5);
        hogs
    }
}

impl Probe for LinuxProbe {
    fn snapshot(&mut self) -> Snapshot {
        let at = Instant::now();
        let elapsed = self.at.map(|before| (at - before).as_secs_f32()).filter(|secs| *secs >= 0.2);
        let times = read("/proc/stat").and_then(|text| parse::cpu_times(&text));
        let busy = match (self.cpu, times) {
            (Some((busy0, total0)), Some((busy1, total1))) if total1 > total0 => {
                Some((busy1 - busy0) as f32 / (total1 - total0) as f32)
            }
            _ => None,
        };
        // Rates need a real interval; two readings in quick succession keep the older baseline.
        let hogs = if self.at.is_none() || elapsed.is_some() {
            self.cpu = times;
            self.at = Some(at);
            self.hogs(elapsed)
        } else {
            Vec::new()
        };
        Snapshot {
            memory: memory(),
            cpu: Cpu {
                logical: logical_cpus(),
                load1: read("/proc/loadavg").and_then(|text| parse::loadavg(&text)),
                busy,
            },
            power: power(),
            gpus: gpus(),
            hogs,
            cgroup: cgroup(),
            self_rss_bytes: process_rss(std::process::id()),
        }
    }
}
