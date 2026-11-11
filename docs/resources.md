# Resource management

The premise: the computer belongs to its user and to whatever they are doing. Lumbre takes what
is spare, says what it took, and gives it back.

It does **not** promise zero impact. A language model on a CPU uses memory and cores while it
runs; what Lumbre controls is when, how much, and for how long.

## What is measured

`lumbre-resources::Probe` returns a `Snapshot`. A value a platform cannot provide is `None` or
`Unknown` — never an assumption.

| Measurement | Linux | macOS | Windows |
| --- | --- | --- | --- |
| Total / available memory | `/proc/meminfo` (`MemAvailable`) | `hw.memsize`, Mach `host_statistics64` (free + inactive + purgeable) | — |
| Memory pressure | PSI, `/proc/pressure/memory` (`some` and `full`, 10 s average) | `kern.memorystatus_vm_pressure_level` | — |
| CPU | `/proc/loadavg`, busy share from `/proc/stat` | load average | cores only |
| Busy processes | `/proc/<pid>/stat`, CPU time between samples; ≥ 0.5 core | — | — |
| Power | `/sys/class/power_supply` (mains / battery, charge) | — | — |
| cgroup limit | cgroup v2 `memory.max`, `memory.high`, `memory.current` of our own cgroup | n/a | n/a |
| VRAM | `/sys/class/drm/*/device/mem_info_vram_*` (amdgpu and drivers that expose it) | unified memory | — |
| Memory per model | `VmRSS` of the worker process | `proc_pidinfo` of the worker | — |
| Daemon memory | `VmRSS` of itself | `proc_pidinfo` of itself | — |

Missing and known: NVIDIA VRAM (NVML is a proprietary library; it will be an optional adapter),
Intel VRAM, power state and busy processes on macOS, everything on Windows.

Sampling costs something (the busy-process scan reads one small file per process), so nothing is
sampled while idle: a snapshot is taken when a request arrives, when a client asks for status,
and every five seconds **only while a model is loaded**.

PSI thresholds: `some avg10 ≥ 10 %` is *elevated*; `some ≥ 40 %` or `full ≥ 5 %` is *critical*.
They are deliberately conservative — a desktop that stalls a tenth of the time already feels
slow — and are constants in one place, to be tuned with data.

## The budget

`budget(policy, snapshot, held_bytes, own_pids)` is a pure function. It returns the bytes a
model may occupy, a thread count, a verdict and the reasons.

```
reserve   = reserve_mb, or max(2 GiB, 15 % of RAM), capped at half the RAM
headroom  = min(available + held_by_lumbre, total) − reserve
budget    = headroom × share(mode)          auto 0.70 · performance 0.90 · saver 0.40
            then min(max_ram_mb, cgroup memory.max − other usage)
threads   = cores/2 (auto) · 3·cores/4 (performance) · cores/4 (saver), at least 1
```

`held_by_lumbre` is the worker's resident memory: a loaded model must not shrink its own budget.

| Condition | Effect |
| --- | --- |
| Memory pressure elevated | share halved; verdict **degrade** (one tier down, half the threads) |
| Memory pressure critical | verdict **defer**: nothing is loaded |
| On battery (auto) | share capped at 0.55 |
| Battery ≤ 20 % (auto) | share capped at 0.40; **degrade** |
| Other programs using ≥ max(1.5 cores, half the cores) (auto, saver) | **degrade** — this is the "someone is playing a game" case |
| Memory could not be read | **defer** |

Example, the reference machine: 16 GiB with 10 GiB available in auto mode gives
`(10 − 2.4) × 0.7 = 5.3 GiB` and 4 threads on 8 cores. That is a unit test.

Every limit carries a `Reason`, and the interface shows it: "limited because: the system is
short of memory", never a silent slowdown.

## Loading and unloading

- At most **one** model is resident. Switching tier unloads the old model before loading the new.
- A model nobody used for `idle_unload_secs` (300 by default; ×4 in performance, ÷4 in saver) is
  unloaded.
- **Hysteresis.** A model is not dropped on one bad sample: critical pressure must hold for two
  consecutive samples, elevated pressure for three and only when the model has been idle for
  10 s. A freshly loaded model is kept at least 20 s unless pressure is critical.
- **Reload cooldown.** After a pressure unload, nothing is loaded for 30 s; requests get
  `Deferred` with `retry_after_secs`. Without it, a request arriving right after the unload
  would reload the model and recreate the pressure.
- A request under critical pressure is deferred with its reason. It is never queued silently.

These rules are in `Residency` and `LoadGate`, tested with scripted time and pressure.

## Idle cost

Goal: under 20 MiB resident with no model loaded. Measured with `benchmarks/idle.sh`; the
`linux-runtime` CI job repeats the Linux measurement on every run and fails above 20 MiB:

| Platform | Resident after start | After 20 s idle | Threads | CPU time |
| --- | --- | --- | --- | --- |
| macOS, Apple Silicon (release build, 2026-11-11) | 6.0 MiB | 6.9 MiB | 1 | 0.01 s |
| Linux x86_64, Ubuntu 24.04 CI runner (release build, 2026-11-11) | 5.5 MiB | 6.4 MiB | 1 | under 0.01 s |

With systemd socket activation (`integrations/linux/systemd`) the idle cost is zero: systemd
holds the socket, starts `lumbred` on the first connection, and `--exit-on-idle` ends it after a
quiet period with no model loaded. The unit also puts the daemon and its worker in a cgroup with
`MemoryHigh`/`MemoryMax`, a reduced `CPUWeight` and idle I/O priority, so the kernel enforces a
ceiling independently of Lumbre's own arithmetic.

## What is not done

- **Adjusting context and caches while loaded.** The context size is fixed at load time; under
  pressure Lumbre unloads rather than shrinks.
- **Measured memory per model.** The estimate used to decide whether a model fits is a
  conservative heuristic from the file size (see [models.md](models.md)). Real profiles need
  real runs.
- **Latency tracking** beyond per-request first-token and total time.
- **Pressure events.** Linux PSI triggers and macOS dispatch sources would replace polling while
  loaded.
- **Its own cgroup for the worker**, with limits set by the governor instead of the unit file.
- **macOS and Windows adapters** beyond what the table above lists. The design is the same trait
  with `host_statistics`/IOKit on macOS and `GlobalMemoryStatusEx`, job objects and the
  low-memory resource notification on Windows.
