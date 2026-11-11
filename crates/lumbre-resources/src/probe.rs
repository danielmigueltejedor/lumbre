use crate::Snapshot;

/// Reads the machine. Implementations keep the previous counters they need to turn cumulative
/// values into rates, so a probe should be reused rather than recreated per reading.
pub trait Probe: Send {
    fn snapshot(&mut self) -> Snapshot;
}

/// The probe for the running operating system.
///
/// On platforms without an adapter yet (Windows in 0.1) it reports the core count and nothing
/// else; the governor treats unknown memory as "do not load".
pub fn system_probe() -> Box<dyn Probe> {
    #[cfg(target_os = "linux")]
    {
        Box::new(crate::linux::LinuxProbe::default())
    }
    #[cfg(target_os = "macos")]
    {
        Box::new(crate::macos::MacProbe)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Box::new(Unsupported)
    }
}

/// Resident memory of another process (a model worker), if it can be read.
pub fn process_rss(pid: u32) -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        crate::linux::process_rss(pid)
    }
    #[cfg(target_os = "macos")]
    {
        crate::macos::process_rss(pid)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = pid;
        None
    }
}

pub(crate) fn logical_cpus() -> u32 {
    std::thread::available_parallelism().map_or(1, |n| n.get() as u32)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
struct Unsupported;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
impl Probe for Unsupported {
    fn snapshot(&mut self) -> Snapshot {
        Snapshot { cpu: crate::Cpu { logical: logical_cpus(), ..Default::default() }, ..Default::default() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn the_real_machine_reports_sane_values() {
        let snapshot = system_probe().snapshot();
        assert!(snapshot.memory.total_bytes > 256 << 20, "{snapshot:?}");
        assert!(snapshot.memory.available_bytes <= snapshot.memory.total_bytes);
        assert!(snapshot.cpu.logical >= 1);
        let rss = snapshot.self_rss_bytes.expect("own RSS is readable");
        assert!(rss > 0 && rss < snapshot.memory.total_bytes);
        assert!(process_rss(std::process::id()).is_some());
    }
}
