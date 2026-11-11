//! Parsers for the Linux `/proc`, `/sys` and cgroup v2 text formats.
//!
//! They take the file contents, not a path, so they are compiled and tested on every platform.

use crate::Psi;

/// `(MemTotal, MemAvailable)` in bytes from `/proc/meminfo`.
pub fn meminfo(text: &str) -> Option<(u64, u64)> {
    let field = |name: &str| {
        text.lines().find_map(|line| {
            let rest = line.strip_prefix(name)?.strip_prefix(':')?;
            let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
            Some(kb * 1024)
        })
    };
    Some((field("MemTotal")?, field("MemAvailable")?))
}

/// `/proc/pressure/memory`.
pub fn psi(text: &str) -> Option<Psi> {
    let avg10 = |kind: &str| {
        text.lines().find_map(|line| {
            let rest = line.strip_prefix(kind)?;
            rest.split_whitespace().find_map(|kv| kv.strip_prefix("avg10=")?.parse::<f32>().ok())
        })
    };
    // Kernels built without full-stall accounting only print the `some` line.
    Some(Psi { some_avg10: avg10("some")?, full_avg10: avg10("full").unwrap_or(0.0) })
}

/// `(busy, total)` jiffies from the aggregate `cpu` line of `/proc/stat`.
pub fn cpu_times(text: &str) -> Option<(u64, u64)> {
    let line = text.lines().find(|line| line.starts_with("cpu "))?;
    let values: Vec<u64> = line.split_whitespace().skip(1).filter_map(|v| v.parse().ok()).collect();
    if values.len() < 4 {
        return None;
    }
    // user nice system idle iowait irq softirq steal; guest time is already inside user.
    let total: u64 = values.iter().take(8).sum();
    let idle = values[3] + values.get(4).copied().unwrap_or(0);
    Some((total - idle, total))
}

/// One-minute load average from `/proc/loadavg`.
pub fn loadavg(text: &str) -> Option<f32> {
    text.split_whitespace().next()?.parse().ok()
}

/// `(name, utime + stime in clock ticks)` from `/proc/<pid>/stat`.
pub fn pid_stat(text: &str) -> Option<(String, u64)> {
    // The name is in parentheses and may itself contain spaces and parentheses.
    let open = text.find('(')?;
    let close = text.rfind(')')?;
    let name = text.get(open + 1..close)?.to_owned();
    let mut fields = text.get(close + 1..)?.split_whitespace();
    // After the name: state(0) ... utime(11) stime(12).
    let utime: u64 = fields.nth(11)?.parse().ok()?;
    let stime: u64 = fields.next()?.parse().ok()?;
    Some((name, utime + stime))
}

/// `VmRSS` in bytes from `/proc/<pid>/status`.
pub fn status_rss(text: &str) -> Option<u64> {
    text.lines().find_map(|line| {
        let kb: u64 = line.strip_prefix("VmRSS:")?.split_whitespace().next()?.parse().ok()?;
        Some(kb * 1024)
    })
}

/// The cgroup v2 path of a process from `/proc/<pid>/cgroup` (the `0::` line).
pub fn cgroup_path(text: &str) -> Option<&str> {
    text.lines().find_map(|line| line.strip_prefix("0::"))
}

/// A cgroup limit file: a byte count, or `max` for unlimited (`None`).
pub fn cgroup_limit(text: &str) -> Option<u64> {
    text.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Pressure;

    #[test]
    fn meminfo_reads_total_and_available() {
        let text = "MemTotal:       16303428 kB\nMemFree:          812340 kB\nMemAvailable:    9123456 kB\n";
        assert_eq!(meminfo(text), Some((16_303_428 * 1024, 9_123_456 * 1024)));
        assert_eq!(meminfo("MemTotal: 1 kB\n"), None);
    }

    #[test]
    fn psi_reads_both_lines_and_classifies() {
        let text = "some avg10=12.50 avg60=3.00 avg300=1.00 total=123\nfull avg10=1.25 avg60=0.10 avg300=0.00 total=45\n";
        let value = psi(text).unwrap();
        assert_eq!(value, Psi { some_avg10: 12.5, full_avg10: 1.25 });
        assert_eq!(value.level(), Pressure::Elevated);
        assert_eq!(Psi { some_avg10: 0.0, full_avg10: 0.0 }.level(), Pressure::Normal);
        assert_eq!(Psi { some_avg10: 8.0, full_avg10: 6.0 }.level(), Pressure::Critical);
        assert_eq!(psi("some avg10=0.00 avg60=0.00 avg300=0.00 total=0\n").unwrap().full_avg10, 0.0);
    }

    #[test]
    fn cpu_times_split_busy_from_idle() {
        let text = "cpu  100 0 50 800 50 0 0 0 0 0\ncpu0 1 2 3 4\n";
        assert_eq!(cpu_times(text), Some((150, 1000)));
    }

    #[test]
    fn pid_stat_survives_awkward_names() {
        let text = "4242 (Web Content (x)) S 1 2 3 4 5 6 7 8 9 10 700 300 0 0 20 0 1 0 100 200 300";
        assert_eq!(pid_stat(text), Some(("Web Content (x)".to_owned(), 1000)));
    }

    #[test]
    fn small_files() {
        assert_eq!(loadavg("1.52 0.80 0.40 2/1234 999\n"), Some(1.52));
        assert_eq!(status_rss("Name:\tx\nVmRSS:\t   12345 kB\n"), Some(12_345 * 1024));
        assert_eq!(
            cgroup_path("0::/user.slice/user-1000.slice/app.scope\n"),
            Some("/user.slice/user-1000.slice/app.scope")
        );
        assert_eq!(cgroup_limit("max\n"), None);
        assert_eq!(cgroup_limit("8589934592\n"), Some(8_589_934_592));
    }
}
