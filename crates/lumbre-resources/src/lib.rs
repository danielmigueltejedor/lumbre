//! What the machine is doing right now, and how much of it Lumbre may take.
//!
//! Two halves:
//!
//! * [`Probe`] reads live measurements (memory, memory pressure, CPU, power, VRAM, busy
//!   processes). Each operating system has its own adapter; values a platform cannot provide are
//!   `None`/`Unknown`, never guessed.
//! * [`budget`] and [`Residency`] are pure functions of those measurements and a [`Policy`]. They
//!   decide how many bytes a model may occupy, how many threads to use, and when a loaded model
//!   has to go. Being pure, they are tested on every platform.

mod governor;
pub mod parse;
mod probe;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

pub use governor::{
    budget, Budget, LoadGate, Mode, Policy, Reason, Residency, ResidencyDecision, UnloadReason, Verdict,
};
pub use probe::{process_rss, system_probe, Probe};

use serde::{Deserialize, Serialize};

/// One reading of the machine. Byte counts are bytes, fractions are 0..=1 unless stated.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub memory: Memory,
    pub cpu: Cpu,
    pub power: Power,
    /// Dedicated GPUs whose VRAM could be read. Empty when none was found or the driver does not
    /// expose it (NVIDIA needs NVML, which 0.1 does not link).
    pub gpus: Vec<Gpu>,
    /// Processes that used at least half a core since the previous snapshot, busiest first.
    pub hogs: Vec<Hog>,
    /// The memory controller of the cgroup this process runs in (Linux, cgroup v2).
    pub cgroup: Option<CgroupMemory>,
    /// Resident memory of the calling process.
    pub self_rss_bytes: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Memory {
    pub total_bytes: u64,
    /// Memory the kernel says can be handed out without swapping (`MemAvailable` on Linux).
    pub available_bytes: u64,
    pub pressure: Pressure,
    /// Raw pressure-stall information, where the kernel provides it (Linux 4.20+).
    pub psi: Option<Psi>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pressure {
    /// The platform gives no pressure signal.
    #[default]
    Unknown,
    Normal,
    Elevated,
    Critical,
}

/// Share of the last ten seconds in which tasks stalled waiting for memory, in percent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Psi {
    /// At least one task stalled.
    pub some_avg10: f32,
    /// All non-idle tasks stalled at once.
    pub full_avg10: f32,
}

impl Psi {
    /// Thresholds are deliberately conservative: a desktop that stalls 10 % of the time already
    /// feels slow to its user.
    pub fn level(self) -> Pressure {
        if self.full_avg10 >= 5.0 || self.some_avg10 >= 40.0 {
            Pressure::Critical
        } else if self.some_avg10 >= 10.0 {
            Pressure::Elevated
        } else {
            Pressure::Normal
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Cpu {
    pub logical: u32,
    pub load1: Option<f32>,
    /// Busy share of all cores since the previous snapshot. `None` on the first one.
    pub busy: Option<f32>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum Power {
    #[default]
    Unknown,
    Ac,
    Battery {
        percent: Option<u8>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Gpu {
    pub name: String,
    pub vram_total_bytes: u64,
    pub vram_used_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Hog {
    pub pid: u32,
    pub name: String,
    /// Cores in use: 1.0 is one core fully busy.
    pub cores: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CgroupMemory {
    pub current_bytes: u64,
    /// `memory.max`; `None` when unlimited.
    pub max_bytes: Option<u64>,
    /// `memory.high`; `None` when unlimited.
    pub high_bytes: Option<u64>,
}
