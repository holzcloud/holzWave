//! Helpers of the performance measurements (POL-02, D-14): the `PERF` line printer, the statistics and the resident-memory
//! reader.
//!
//! `std` only and nothing that needs a safety proof, so that another crate's test can include it by path:
//!
//! ```ignore
//! #[path = "../../core/tests/perf_report/mod.rs"]
//! mod perf_report;
//! ```
//!
//! A measurement prints one line per metric and writes no file: `PERF {"name":..,"unit":..,"runs":..,"median":..,"p95":..}`.
//! `scripts/perf-collect.mjs` turns the lines of a log into the run file that `scripts/perf-compare.mjs` judges.
#![allow(dead_code)]

#[cfg(target_os = "macos")]
use std::process::Command;

/// The median of `values`: the middle one, the mean of the two middle ones for an even count. Not sorted by the caller.
pub fn median(values: &[f64]) -> f64 {
    assert!(!values.is_empty(), "the median of no values");
    let mut v = values.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).expect("a measurement is never NaN"));
    let n = v.len();
    if n % 2 == 1 { v[n / 2] } else { (v[n / 2 - 1] + v[n / 2]) / 2.0 }
}

/// The nearest-rank percentile: the value at rank `ceil(p / 100 * n)` of the sorted values. `p` is in `1..=100`.
pub fn percentile(values: &[f64], p: f64) -> f64 {
    assert!(!values.is_empty(), "a percentile of no values");
    assert!((1.0..=100.0).contains(&p), "percentile {p} is outside 1..=100");
    let mut v = values.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).expect("a measurement is never NaN"));
    let rank = ((p / 100.0) * v.len() as f64).ceil() as usize;
    v[rank.clamp(1, v.len()) - 1]
}

/// Print the `PERF` line of one metric from the value of each run. A value that is not a positive finite number is a
/// failed measurement, so it panics here instead of printing a line that the collector would have to refuse.
///
/// The line starts with a newline: the test harness prints `test name ... ` without one, and a `PERF` line must be at
/// the start of its line.
pub fn report(name: &str, unit: &str, values: &[f64]) {
    assert!(!values.is_empty(), "{name}: no run");
    for v in values {
        assert!(v.is_finite() && *v > 0.0, "{name}: a run measured {v}, which is not a positive finite number");
    }
    assert!(name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'.'), "bad name {name}");
    println!(
        "\nPERF {{\"name\":\"{name}\",\"unit\":\"{unit}\",\"runs\":{},\"median\":{},\"p95\":{}}}",
        values.len(),
        median(values),
        percentile(values, 95.0)
    );
}

/// The resident set of this process in KiB: `ps -o rss= -p <pid>` on macOS, the `VmRSS` line of `/proc/self/status` on
/// Linux, `None` anywhere else or when it cannot be read.
pub fn rss_kib() -> Option<u64> {
    #[cfg(target_os = "macos")]
    {
        let out = Command::new("/bin/ps").args(["-o", "rss=", "-p", &std::process::id().to_string()]).output().ok()?;
        String::from_utf8(out.stdout).ok()?.trim().parse().ok()
    }
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        let line = status.lines().find(|l| l.starts_with("VmRSS:"))?;
        line.split_whitespace().nth(1)?.parse().ok()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        None
    }
}
