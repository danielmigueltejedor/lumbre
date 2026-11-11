//! The rules that turn measurements into limits. Pure: no clocks or I/O of their own.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::{Power, Pressure, Snapshot};

const GIB: u64 = 1 << 30;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Yield to whatever else the user is doing.
    #[default]
    Auto,
    /// The user asked for speed: larger share of memory, more threads, slower unloading.
    Performance,
    /// Smallest footprint: small share of memory, few threads, quick unloading.
    Saver,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Policy {
    pub mode: Mode,
    /// Hard ceiling for model memory set by the user.
    pub max_ram_bytes: Option<u64>,
    /// Memory always left to the rest of the system. Default: 15 % of RAM, at least 2 GiB.
    pub reserve_bytes: Option<u64>,
    /// A model nobody used for this long is unloaded (scaled by the mode).
    pub idle_unload: Duration,
    /// A freshly loaded model is kept at least this long unless pressure is critical.
    pub min_resident: Duration,
    /// After unloading because of pressure, no model is loaded for this long.
    pub reload_cooldown: Duration,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            mode: Mode::Auto,
            max_ram_bytes: None,
            reserve_bytes: None,
            idle_unload: Duration::from_secs(300),
            min_resident: Duration::from_secs(20),
            reload_cooldown: Duration::from_secs(30),
        }
    }
}

impl Policy {
    pub fn effective_idle_unload(&self) -> Duration {
        match self.mode {
            Mode::Auto => self.idle_unload,
            Mode::Performance => self.idle_unload * 4,
            Mode::Saver => self.idle_unload / 4,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Nothing in the way.
    Run,
    /// Run, but smaller: the caller should prefer a smaller model or context.
    Degrade,
    /// Do not load or generate now.
    Defer,
}

/// Why the budget is what it is. Shown to the user verbatim, so every limit has a cause.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Reason {
    MemoryUnknown,
    MemoryPressureElevated,
    MemoryPressureCritical,
    /// A model was unloaded because of pressure moments ago; loading waits for the cooldown.
    RecentPressure,
    OnBattery,
    LowBattery {
        percent: u8,
    },
    BusyProcess {
        name: String,
        cores: f32,
    },
    UserLimit {
        bytes: u64,
    },
    CgroupLimit {
        bytes: u64,
    },
    SaverMode,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Budget {
    /// Bytes a model (weights, context and caches together) may occupy.
    pub model_bytes: u64,
    /// Inference threads to use.
    pub threads: u32,
    pub verdict: Verdict,
    pub reasons: Vec<Reason>,
}

/// How much Lumbre may use right now.
///
/// `held_bytes` is the memory Lumbre's models already occupy: it counts as available to Lumbre,
/// otherwise a loaded model would shrink its own budget. `own_pids` are Lumbre's processes, which
/// never count as "the user is busy".
pub fn budget(policy: &Policy, snapshot: &Snapshot, held_bytes: u64, own_pids: &[u32]) -> Budget {
    let memory = &snapshot.memory;
    let logical = snapshot.cpu.logical.max(1);
    let mut reasons = Vec::new();
    let mut verdict = Verdict::Run;
    let mut threads = match policy.mode {
        Mode::Auto => (logical / 2).max(1),
        Mode::Performance => (logical * 3 / 4).max(1),
        Mode::Saver => (logical / 4).max(1),
    };
    let mut share = match policy.mode {
        Mode::Auto => 0.7,
        Mode::Performance => 0.9,
        Mode::Saver => 0.4,
    };
    if policy.mode == Mode::Saver {
        reasons.push(Reason::SaverMode);
    }

    if memory.total_bytes == 0 {
        return Budget {
            model_bytes: 0,
            threads,
            verdict: Verdict::Defer,
            reasons: vec![Reason::MemoryUnknown],
        };
    }

    if let (Mode::Auto, Power::Battery { percent }) = (policy.mode, snapshot.power) {
        match percent {
            Some(percent) if percent <= 20 => {
                reasons.push(Reason::LowBattery { percent });
                share = f64::min(share, 0.4);
                verdict = Verdict::Degrade;
            }
            _ => {
                reasons.push(Reason::OnBattery);
                share = f64::min(share, 0.55);
            }
        }
    }

    if policy.mode != Mode::Performance {
        let foreign: Vec<_> = snapshot.hogs.iter().filter(|hog| !own_pids.contains(&hog.pid)).collect();
        let cores: f32 = foreign.iter().map(|hog| hog.cores).sum();
        if cores >= f32::max(1.5, logical as f32 * 0.5) {
            if let Some(top) = foreign.first() {
                reasons.push(Reason::BusyProcess { name: top.name.clone(), cores: top.cores });
            }
            verdict = Verdict::Degrade;
        }
    }

    match memory.pressure {
        Pressure::Critical => {
            reasons.push(Reason::MemoryPressureCritical);
            verdict = Verdict::Defer;
        }
        Pressure::Elevated => {
            reasons.push(Reason::MemoryPressureElevated);
            share *= 0.5;
            verdict = Verdict::Degrade;
        }
        Pressure::Normal | Pressure::Unknown => {}
    }

    let reserve = policy.reserve_bytes.unwrap_or_else(|| u64::max(2 * GIB, memory.total_bytes * 15 / 100));
    let reserve = reserve.min(memory.total_bytes / 2);
    let headroom = (memory.available_bytes + held_bytes).min(memory.total_bytes).saturating_sub(reserve);
    let mut model_bytes = (headroom as f64 * share) as u64;

    if let Some(limit) = policy.max_ram_bytes {
        if limit < model_bytes {
            model_bytes = limit;
            reasons.push(Reason::UserLimit { bytes: limit });
        }
    }
    if let Some(max) = snapshot.cgroup.and_then(|cgroup| {
        // What the cgroup still allows, counting our own models as reusable.
        cgroup.max_bytes.map(|max| max.saturating_sub(cgroup.current_bytes.saturating_sub(held_bytes)))
    }) {
        if max < model_bytes {
            model_bytes = max;
            reasons.push(Reason::CgroupLimit { bytes: max });
        }
    }

    match verdict {
        Verdict::Defer => model_bytes = 0,
        Verdict::Degrade => threads = (threads / 2).max(1),
        Verdict::Run => {}
    }
    Budget { model_bytes, threads, verdict, reasons }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnloadReason {
    Idle,
    Pressure,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResidencyDecision {
    Keep,
    Unload(UnloadReason),
}

/// Tracks one loaded model and decides when it has to go.
///
/// Unloading is deliberately sticky in both directions so the system does not oscillate: a model
/// is not dropped on a single bad sample, and (via [`LoadGate`]) not reloaded straight after a
/// pressure unload.
#[derive(Clone, Copy, Debug)]
pub struct Residency {
    loaded_at: Instant,
    last_used: Instant,
    elevated_streak: u32,
    critical_streak: u32,
}

impl Residency {
    /// Consecutive critical samples before an unconditional unload.
    const CRITICAL_SAMPLES: u32 = 2;
    /// Consecutive elevated samples before an idle model is unloaded early.
    const ELEVATED_SAMPLES: u32 = 3;
    /// "Idle" for the purpose of an early unload under elevated pressure.
    const BRIEFLY_IDLE: Duration = Duration::from_secs(10);

    pub fn new(now: Instant) -> Self {
        Self { loaded_at: now, last_used: now, elevated_streak: 0, critical_streak: 0 }
    }

    /// The model was just used.
    pub fn touch(&mut self, now: Instant) {
        self.last_used = now;
    }

    pub fn idle_for(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.last_used)
    }

    /// Feed one pressure sample; call at a steady interval while a model is loaded.
    pub fn observe(&mut self, now: Instant, pressure: Pressure, policy: &Policy) -> ResidencyDecision {
        match pressure {
            Pressure::Critical => {
                self.critical_streak += 1;
                self.elevated_streak += 1;
            }
            Pressure::Elevated => {
                self.critical_streak = 0;
                self.elevated_streak += 1;
            }
            Pressure::Normal | Pressure::Unknown => {
                self.critical_streak = 0;
                self.elevated_streak = 0;
            }
        }
        if self.critical_streak >= Self::CRITICAL_SAMPLES {
            return ResidencyDecision::Unload(UnloadReason::Pressure);
        }
        if now.saturating_duration_since(self.loaded_at) < policy.min_resident {
            return ResidencyDecision::Keep;
        }
        let idle = self.idle_for(now);
        if idle >= policy.effective_idle_unload() {
            return ResidencyDecision::Unload(UnloadReason::Idle);
        }
        if self.elevated_streak >= Self::ELEVATED_SAMPLES && idle >= Self::BRIEFLY_IDLE {
            return ResidencyDecision::Unload(UnloadReason::Pressure);
        }
        ResidencyDecision::Keep
    }
}

/// Refuses loads for a while after a pressure unload.
#[derive(Clone, Copy, Debug, Default)]
pub struct LoadGate {
    closed_until: Option<Instant>,
}

impl LoadGate {
    pub fn close(&mut self, now: Instant, policy: &Policy) {
        self.closed_until = Some(now + policy.reload_cooldown);
    }

    /// Time left until loads are allowed again, or `None` when open.
    pub fn closed_for(&self, now: Instant) -> Option<Duration> {
        self.closed_until.and_then(|until| until.checked_duration_since(now)).filter(|left| !left.is_zero())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CgroupMemory, Cpu, Hog, Memory};

    fn machine(total_gib: u64, available_gib: u64) -> Snapshot {
        Snapshot {
            memory: Memory {
                total_bytes: total_gib * GIB,
                available_bytes: available_gib * GIB,
                pressure: Pressure::Normal,
                psi: None,
            },
            cpu: Cpu { logical: 8, load1: None, busy: None },
            power: Power::Ac,
            ..Default::default()
        }
    }

    #[test]
    fn a_16_gib_machine_with_10_free_leaves_room_for_a_4b_model() {
        let result = budget(&Policy::default(), &machine(16, 10), 0, &[]);
        // reserve = max(2 GiB, 15 % of 16 GiB = 2.4 GiB); (10 - 2.4) * 0.7 = 5.32 GiB
        assert_eq!(result.verdict, Verdict::Run);
        assert!(result.reasons.is_empty());
        let gib = result.model_bytes as f64 / GIB as f64;
        assert!((gib - 5.32).abs() < 0.01, "{gib}");
        assert_eq!(result.threads, 4);
    }

    #[test]
    fn the_budget_never_exceeds_what_is_available_minus_the_reserve() {
        for mode in [Mode::Auto, Mode::Performance, Mode::Saver] {
            for available in 0..=16 {
                let policy = Policy { mode, ..Default::default() };
                let snapshot = machine(16, available);
                let result = budget(&policy, &snapshot, 0, &[]);
                assert!(
                    result.model_bytes <= (available * GIB).saturating_sub(2 * GIB),
                    "{mode:?} {available}"
                );
            }
        }
    }

    #[test]
    fn memory_already_held_by_lumbre_does_not_shrink_its_own_budget() {
        let empty = budget(&Policy::default(), &machine(16, 10), 0, &[]);
        // The same machine after loading a 3 GiB model: 7 available, 3 held.
        let loaded = budget(&Policy::default(), &machine(16, 7), 3 * GIB, &[]);
        assert_eq!(empty.model_bytes, loaded.model_bytes);
    }

    #[test]
    fn user_and_cgroup_limits_cap_the_budget_and_say_so() {
        let policy = Policy { max_ram_bytes: Some(3 * GIB), ..Default::default() };
        let result = budget(&policy, &machine(32, 24), 0, &[]);
        assert_eq!(result.model_bytes, 3 * GIB);
        assert_eq!(result.reasons, vec![Reason::UserLimit { bytes: 3 * GIB }]);

        let mut snapshot = machine(32, 24);
        snapshot.cgroup =
            Some(CgroupMemory { current_bytes: GIB, max_bytes: Some(3 * GIB), high_bytes: None });
        let result = budget(&Policy::default(), &snapshot, 0, &[]);
        assert_eq!(result.model_bytes, 2 * GIB);
        assert_eq!(result.reasons, vec![Reason::CgroupLimit { bytes: 2 * GIB }]);
    }

    #[test]
    fn pressure_degrades_then_defers() {
        let mut snapshot = machine(16, 10);
        let normal = budget(&Policy::default(), &snapshot, 0, &[]);
        snapshot.memory.pressure = Pressure::Elevated;
        let elevated = budget(&Policy::default(), &snapshot, 0, &[]);
        assert_eq!(elevated.verdict, Verdict::Degrade);
        assert_eq!(elevated.model_bytes, normal.model_bytes / 2);
        assert_eq!(elevated.threads, 2);
        snapshot.memory.pressure = Pressure::Critical;
        let critical = budget(&Policy::default(), &snapshot, 0, &[]);
        assert_eq!((critical.verdict, critical.model_bytes), (Verdict::Defer, 0));
        assert_eq!(critical.reasons, vec![Reason::MemoryPressureCritical]);
    }

    #[test]
    fn a_game_using_half_the_cores_degrades_but_lumbre_itself_does_not() {
        let mut snapshot = machine(16, 10);
        snapshot.hogs = vec![Hog { pid: 77, name: "game".into(), cores: 4.5 }];
        let result = budget(&Policy::default(), &snapshot, 0, &[]);
        assert_eq!(result.verdict, Verdict::Degrade);
        assert_eq!(result.reasons, vec![Reason::BusyProcess { name: "game".into(), cores: 4.5 }]);
        assert_eq!(budget(&Policy::default(), &snapshot, 0, &[77]).verdict, Verdict::Run);
        let performance = Policy { mode: Mode::Performance, ..Default::default() };
        assert_eq!(budget(&performance, &snapshot, 0, &[]).verdict, Verdict::Run);
    }

    #[test]
    fn battery_reduces_the_share_in_auto_only() {
        let mut snapshot = machine(16, 10);
        let mains = budget(&Policy::default(), &snapshot, 0, &[]);
        snapshot.power = Power::Battery { percent: Some(60) };
        let battery = budget(&Policy::default(), &snapshot, 0, &[]);
        assert!(battery.model_bytes < mains.model_bytes);
        assert_eq!(battery.verdict, Verdict::Run);
        snapshot.power = Power::Battery { percent: Some(12) };
        let low = budget(&Policy::default(), &snapshot, 0, &[]);
        assert_eq!(low.verdict, Verdict::Degrade);
        assert_eq!(low.reasons, vec![Reason::LowBattery { percent: 12 }]);
        let performance = Policy { mode: Mode::Performance, ..Default::default() };
        assert!(budget(&performance, &snapshot, 0, &[]).reasons.is_empty());
    }

    #[test]
    fn unknown_memory_defers() {
        let result = budget(&Policy::default(), &Snapshot::default(), 0, &[]);
        assert_eq!((result.verdict, result.model_bytes), (Verdict::Defer, 0));
    }

    #[test]
    fn an_idle_model_is_unloaded_after_the_timeout_and_not_before() {
        let policy = Policy::default();
        let start = Instant::now();
        let mut residency = Residency::new(start);
        let at = |secs| start + Duration::from_secs(secs);
        assert_eq!(residency.observe(at(299), Pressure::Normal, &policy), ResidencyDecision::Keep);
        assert_eq!(
            residency.observe(at(300), Pressure::Normal, &policy),
            ResidencyDecision::Unload(UnloadReason::Idle)
        );
        residency.touch(at(300));
        assert_eq!(residency.observe(at(500), Pressure::Normal, &policy), ResidencyDecision::Keep);
    }

    #[test]
    fn one_bad_sample_does_not_unload_but_a_streak_does() {
        let policy = Policy::default();
        let start = Instant::now();
        let at = |secs| start + Duration::from_secs(secs);

        let mut residency = Residency::new(start);
        assert_eq!(residency.observe(at(30), Pressure::Critical, &policy), ResidencyDecision::Keep);
        assert_eq!(residency.observe(at(32), Pressure::Normal, &policy), ResidencyDecision::Keep);
        assert_eq!(residency.observe(at(34), Pressure::Critical, &policy), ResidencyDecision::Keep);
        assert_eq!(
            residency.observe(at(36), Pressure::Critical, &policy),
            ResidencyDecision::Unload(UnloadReason::Pressure)
        );

        // Elevated pressure only evicts a model that is at least briefly idle.
        let mut residency = Residency::new(start);
        for secs in [30, 32, 34, 36] {
            residency.touch(at(secs));
            assert_eq!(residency.observe(at(secs), Pressure::Elevated, &policy), ResidencyDecision::Keep);
        }
        assert_eq!(
            residency.observe(at(47), Pressure::Elevated, &policy),
            ResidencyDecision::Unload(UnloadReason::Pressure)
        );
    }

    #[test]
    fn critical_pressure_overrides_the_minimum_residency() {
        let policy = Policy::default();
        let start = Instant::now();
        let mut residency = Residency::new(start);
        residency.observe(start + Duration::from_secs(1), Pressure::Critical, &policy);
        assert_eq!(
            residency.observe(start + Duration::from_secs(2), Pressure::Critical, &policy),
            ResidencyDecision::Unload(UnloadReason::Pressure)
        );
    }

    #[test]
    fn the_gate_stays_closed_for_the_cooldown() {
        let policy = Policy::default();
        let start = Instant::now();
        let mut gate = LoadGate::default();
        assert_eq!(gate.closed_for(start), None);
        gate.close(start, &policy);
        assert_eq!(gate.closed_for(start + Duration::from_secs(10)), Some(Duration::from_secs(20)));
        assert_eq!(gate.closed_for(start + Duration::from_secs(30)), None);
    }

    #[test]
    fn modes_scale_the_idle_timeout() {
        let saver = Policy { mode: Mode::Saver, ..Default::default() };
        let performance = Policy { mode: Mode::Performance, ..Default::default() };
        assert_eq!(saver.effective_idle_unload(), Duration::from_secs(75));
        assert_eq!(performance.effective_idle_unload(), Duration::from_secs(1200));
    }
}
