//! macOS adapter: Mach host statistics and sysctl.
//!
//! 0.1 reads memory, the kernel's memory-pressure level, load and process RSS. Power state and
//! per-process CPU are not read yet (they need IOKit and `proc_pid_rusage` sampling), so
//! `power` is `Unknown` and `hogs` is empty here.
#![allow(unsafe_code)]

use std::ffi::CStr;
use std::mem;

use crate::probe::{logical_cpus, Probe};
use crate::{Cpu, Memory, Pressure, Snapshot};

#[derive(Debug)]
pub(crate) struct MacProbe;

fn sysctl<T: Copy + Default>(name: &CStr) -> Option<T> {
    let mut value = T::default();
    let mut len = mem::size_of::<T>();
    // SAFETY: `value` is a plain-old-data out buffer of exactly `len` bytes and `name` is
    // NUL-terminated; the kernel writes at most `len` bytes.
    let rc = unsafe {
        libc::sysctlbyname(name.as_ptr(), (&raw mut value).cast(), &raw mut len, std::ptr::null_mut(), 0)
    };
    (rc == 0 && len == mem::size_of::<T>()).then_some(value)
}

fn available_bytes() -> Option<u64> {
    // SAFETY: `vm_statistics64` is plain old data; zeroed is a valid value for an out buffer.
    let mut stats: libc::vm_statistics64 = unsafe { mem::zeroed() };
    let mut count = libc::HOST_VM_INFO64_COUNT;
    // SAFETY: the buffer and its size in `integer_t` units match what HOST_VM_INFO64 expects.
    #[allow(deprecated)]
    let rc = unsafe {
        libc::host_statistics64(
            libc::mach_host_self(),
            libc::HOST_VM_INFO64,
            (&raw mut stats).cast(),
            &raw mut count,
        )
    };
    if rc != libc::KERN_SUCCESS {
        return None;
    }
    // SAFETY: sysconf has no preconditions.
    let page = u64::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).ok()?;
    // Free pages plus what the kernel can reclaim without swapping: inactive and purgeable.
    let pages =
        u64::from(stats.free_count) + u64::from(stats.inactive_count) + u64::from(stats.purgeable_count);
    Some(pages * page)
}

fn pressure() -> Pressure {
    // 1 normal, 2 warning, 4 critical (see `kern.memorystatus_vm_pressure_level`).
    match sysctl::<i32>(c"kern.memorystatus_vm_pressure_level") {
        Some(1) => Pressure::Normal,
        Some(2) => Pressure::Elevated,
        Some(4) => Pressure::Critical,
        _ => Pressure::Unknown,
    }
}

fn load1() -> Option<f32> {
    let mut load = [0f64; 1];
    // SAFETY: the buffer holds one element and we ask for one.
    (unsafe { libc::getloadavg(load.as_mut_ptr(), 1) } == 1).then_some(load[0] as f32)
}

pub(crate) fn process_rss(pid: u32) -> Option<u64> {
    // SAFETY: `proc_taskinfo` is plain old data; zeroed is a valid out buffer.
    let mut info: libc::proc_taskinfo = unsafe { mem::zeroed() };
    let size = i32::try_from(mem::size_of::<libc::proc_taskinfo>()).ok()?;
    // SAFETY: the buffer is `size` bytes long, which is what PROC_PIDTASKINFO fills.
    let written = unsafe {
        libc::proc_pidinfo(i32::try_from(pid).ok()?, libc::PROC_PIDTASKINFO, 0, (&raw mut info).cast(), size)
    };
    (written == size).then_some(info.pti_resident_size)
}

impl Probe for MacProbe {
    fn snapshot(&mut self) -> Snapshot {
        Snapshot {
            memory: Memory {
                total_bytes: sysctl::<u64>(c"hw.memsize").unwrap_or(0),
                available_bytes: available_bytes().unwrap_or(0),
                pressure: pressure(),
                psi: None,
            },
            cpu: Cpu { logical: logical_cpus(), load1: load1(), busy: None },
            self_rss_bytes: process_rss(std::process::id()),
            ..Default::default()
        }
    }
}
