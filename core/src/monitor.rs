//! The system monitor block: what this computer is doing, sampled on demand.
//!
//! One [`Monitor`] keeps the operating system's counters between two samples (CPU use and network traffic are differences,
//! so one reading alone says nothing). Every monitor pane asks the same instance; a sample younger than half a second is
//! given again instead of measured again, so a few panes cost no more than one. Only numbers and process names leave
//! this module, never a command line (they hold secrets), and the list of processes is cut to the busiest few.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;
use sysinfo::{MemoryRefreshKind, Networks, ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System};

/// A sample younger than this is given again.
pub const MIN_INTERVAL: Duration = Duration::from_millis(500);
/// How many processes a sample lists.
pub const TOP: usize = 8;
/// The longest process name sent.
const NAME_MAX: usize = 40;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Proc {
    pub pid: u32,
    pub name: String,
    /// Percent of one core, so it can pass 100.
    pub cpu: f32,
    /// Bytes of resident memory.
    pub mem: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Snapshot {
    /// Percent of all cores, 0 to 100.
    pub cpu: f32,
    pub cores: usize,
    pub mem_used: u64,
    pub mem_total: u64,
    /// Bytes per second since the last sample, summed over all interfaces.
    pub net_rx: u64,
    pub net_tx: u64,
    /// Seconds since the computer started.
    pub uptime: u64,
    pub procs: Vec<Proc>,
}

struct State {
    sys: System,
    nets: Networks,
    at: Option<Instant>,
    traffic: (u64, u64),
    last: Option<Snapshot>,
}

/// The sampler, shared by every monitor pane.
pub struct Monitor(Mutex<State>);

impl Default for Monitor {
    fn default() -> Self {
        Self::new()
    }
}

/// A name that is safe to show: no control characters, and short.
fn clean(name: &str) -> String {
    name.chars().filter(|c| !c.is_control()).take(NAME_MAX).collect()
}

/// The busiest `top` of `procs`, by CPU and then memory.
pub fn busiest(mut procs: Vec<Proc>, top: usize) -> Vec<Proc> {
    procs.sort_by(|a, b| b.cpu.total_cmp(&a.cpu).then(b.mem.cmp(&a.mem)).then(a.pid.cmp(&b.pid)));
    procs.truncate(top);
    procs
}

/// Bytes per second from a counter that was `before` and is `now` after `secs` seconds. A counter that went backwards
/// (an interface came and went) is zero, not a huge number.
pub fn rate(before: u64, now: u64, secs: f64) -> u64 {
    if secs <= 0.0 || now < before { 0 } else { ((now - before) as f64 / secs) as u64 }
}

impl Monitor {
    pub fn new() -> Self {
        let sys = System::new_with_specifics(RefreshKind::nothing().with_memory(MemoryRefreshKind::nothing().with_ram()));
        let nets = Networks::new_with_refreshed_list();
        Self(Mutex::new(State { sys, nets, at: None, traffic: (0, 0), last: None }))
    }

    /// Measure now, or give the sample of the last half second. Blocks for a few milliseconds.
    pub fn sample(&self) -> Snapshot {
        let mut s = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        if let (Some(at), Some(last)) = (s.at, &s.last)
            && now.duration_since(at) < MIN_INTERVAL
        {
            return last.clone();
        }
        s.sys.refresh_cpu_usage();
        s.sys.refresh_memory();
        s.sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing().with_cpu().with_memory(),
        );
        s.nets.refresh(true);
        let (rx, tx) = s.nets.iter().fold((0u64, 0u64), |(r, t), (_, n)| (r + n.total_received(), t + n.total_transmitted()));
        let secs = s.at.map_or(0.0, |at| now.duration_since(at).as_secs_f64());
        let (net_rx, net_tx) = if s.at.is_some() { (rate(s.traffic.0, rx, secs), rate(s.traffic.1, tx, secs)) } else { (0, 0) };
        s.traffic = (rx, tx);
        s.at = Some(now);
        let procs = s
            .sys
            .processes()
            .iter()
            .map(|(pid, p)| Proc { pid: pid.as_u32(), name: clean(&p.name().to_string_lossy()), cpu: p.cpu_usage(), mem: p.memory() })
            .collect();
        let snapshot = Snapshot {
            cpu: s.sys.global_cpu_usage().clamp(0.0, 100.0),
            cores: s.sys.cpus().len(),
            mem_used: s.sys.used_memory(),
            mem_total: s.sys.total_memory(),
            net_rx,
            net_tx,
            uptime: System::uptime(),
            procs: busiest(procs, TOP),
        };
        s.last = Some(snapshot.clone());
        snapshot
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(pid: u32, cpu: f32, mem: u64) -> Proc {
        Proc { pid, name: format!("p{pid}"), cpu, mem }
    }

    #[test]
    fn the_busiest_processes_come_first_and_the_list_is_cut() {
        let got = busiest(vec![p(1, 5.0, 10), p(2, 50.0, 1), p(3, 5.0, 99), p(4, 0.0, 1000)], 3);
        assert_eq!(got.iter().map(|x| x.pid).collect::<Vec<_>>(), vec![2, 3, 1]);
        assert!(busiest(vec![], 3).is_empty());
    }

    #[test]
    fn a_rate_is_bytes_per_second_and_never_negative() {
        assert_eq!(rate(1000, 3000, 2.0), 1000);
        assert_eq!(rate(3000, 1000, 2.0), 0, "a counter that went backwards");
        assert_eq!(rate(0, 100, 0.0), 0);
    }

    #[test]
    fn names_lose_control_characters_and_are_short() {
        assert_eq!(clean("a\x1b[31mb\n"), "a[31mb");
        assert_eq!(clean(&"x".repeat(100)).len(), NAME_MAX);
    }

    #[test]
    fn a_sample_of_this_computer_is_sane_and_a_quick_second_is_the_same_one() {
        let m = Monitor::new();
        let a = m.sample();
        assert!(a.cores >= 1 && a.mem_total > 0 && a.mem_used <= a.mem_total);
        assert!((0.0..=100.0).contains(&a.cpu));
        assert!(a.procs.len() <= TOP && !a.procs.is_empty());
        assert_eq!((a.net_rx, a.net_tx), (0, 0), "the first sample has no earlier one to compare with");
        assert_eq!(m.sample(), a, "within half a second the sample is given again");
        std::thread::sleep(MIN_INTERVAL + Duration::from_millis(50));
        let c = m.sample();
        assert!(c.uptime >= a.uptime);
    }
}
