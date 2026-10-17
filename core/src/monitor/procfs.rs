//! The reader of what the fixed command of [`super::remote`] prints on a Linux host.
//!
//! Everything here is pure: no input or output, no clock, no network. The text comes from another computer and is not
//! trusted, so the reader never believes its layout: the sections are taken from the end of the text (a process name
//! cannot forge one, because a section marker is read from its last occurrence), every counter is added, multiplied and
//! subtracted with saturating arithmetic, no division can be by zero, and lines and counts are bounded. Nothing here
//! panics on host data: there is no indexing without a bound check and no unwrap of a value that came from the host.

use std::collections::HashMap;

use super::{Proc, Snapshot, TOP, busiest, clean, rate};

/// The most process lines read; the rest are ignored.
pub(super) const MAX_PROCS: usize = 20_000;
/// A line longer than this is skipped.
pub(super) const MAX_LINE: usize = 4096;
/// The longest system word kept.
const SYSTEM_MAX: usize = 24;

pub(super) const BEGIN: &str = "@@begin";
pub(super) const UNAME: &str = "@@uname";
pub(super) const UPTIME: &str = "@@uptime";
pub(super) const STAT: &str = "@@stat";
pub(super) const MEM: &str = "@@mem";
pub(super) const NET: &str = "@@net";
pub(super) const PAGE: &str = "@@page";
pub(super) const END: &str = "@@end";

/// The answer ended before its last marker.
pub(super) const CUT_SHORT: &str = "the answer of the host was cut short";
/// A section is missing or holds no usable number.
pub(super) const UNREADABLE: &str = "the counters of the host could not be read";

/// The page size used when the host does not say a usable one.
const DEFAULT_PAGE: u64 = 4096;
/// The page sizes believed: a power of two between these.
const PAGE_RANGE: std::ops::RangeInclusive<u64> = 1024..=2_097_152;

/// One process as the host's per-process stat file says.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct RawProc {
    pub pid: u32,
    /// When the process started, in ticks after boot: with the pid it tells a process from a new one that took the number.
    pub start: u64,
    pub name: String,
    /// User plus system ticks.
    pub ticks: u64,
    pub rss_pages: u64,
}

/// The counters of one reading.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Raw {
    pub uptime: f64,
    pub cpu_total: u64,
    pub cpu_idle: u64,
    pub cores: usize,
    pub mem_total_kb: u64,
    pub mem_available_kb: u64,
    pub rx: u64,
    pub tx: u64,
    pub page: u64,
    pub procs: Vec<RawProc>,
}

/// What a host answered.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Parsed {
    Linux(Raw),
    /// Another system: its own word for itself, cleaned (empty when it named none).
    Other(String),
}

/// The word a host gave for its system: letters, digits, dot, dash and underscore only, at most 24 characters.
pub(super) fn clean_system(word: &str) -> String {
    word.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')).take(SYSTEM_MAX).collect()
}

/// The lines of `text` with the byte offset of each one's start. The marker lines are compared whole, without a trailing
/// carriage return.
fn lines_with_offsets(text: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut at = 0;
    for raw in text.split_inclusive('\n') {
        out.push((at, raw.trim_end_matches(['\n', '\r'])));
        at += raw.len();
    }
    out
}

/// The offset of the last line before `limit` that is exactly `marker`.
fn last_marker(lines: &[(usize, &str)], marker: &str, limit: usize) -> Option<usize> {
    lines.iter().rev().filter(|(at, _)| *at < limit).find(|(_, line)| *line == marker).map(|(at, _)| *at)
}

/// The text of the section that starts at `at` (after its marker line) and ends at `end`.
fn section(text: &str, at: usize, end: usize) -> &str {
    let body = text.get(at..end).unwrap_or("");
    body.split_once('\n').map_or("", |(_, rest)| rest)
}

/// Read the host's answer. The markers are searched from the end, one after the other, each only before the one found
/// before it, so nothing in the process list can stand in for a section. A text without `@@uname` is a host that ran
/// none of the command (no POSIX shell): `Other("")`. Any other system word is `Other(word)` whatever follows it.
pub(super) fn parse(output: &str) -> Result<Parsed, &'static str> {
    let lines = lines_with_offsets(output);
    let mut limit = output.len();
    let mut found: [Option<usize>; 7] = [None; 7];
    // The order of the command is uname, uptime, stat, mem, net, page, end: search backwards.
    let markers = [UNAME, UPTIME, STAT, MEM, NET, PAGE, END];
    for (slot, marker) in found.iter_mut().zip(markers).rev() {
        *slot = last_marker(&lines, marker, limit);
        if let Some(at) = *slot {
            limit = at;
        }
    }
    let [uname, uptime, stat, mem, net, page, end] = found;
    let Some(uname_at) = uname else { return Ok(Parsed::Other(String::new())) };
    // The section of a marker ends where the next found one starts, or at the end of the text.
    let next = |at: usize| found.iter().flatten().copied().filter(|other| *other > at).min().unwrap_or(output.len());
    let word = section(output, uname_at, next(uname_at)).lines().next().unwrap_or("");
    let word = clean_system(word);
    if word != "Linux" {
        return Ok(Parsed::Other(word));
    }
    let (Some(uptime), Some(stat), Some(mem), Some(net), Some(page), Some(_)) = (uptime, stat, mem, net, page, end) else {
        return Err(if end.is_none() { CUT_SHORT } else { UNREADABLE });
    };
    let uptime = parse_uptime(section(output, uptime, next(uptime)))?;
    let (cpu_total, cpu_idle, cores) = parse_cpu(section(output, stat, next(stat)))?;
    let (mem_total_kb, mem_available_kb) = parse_mem(section(output, mem, next(mem)))?;
    let (rx, tx) = parse_net(section(output, net, next(net)));
    let page = parse_page(section(output, page, next(page)));
    let procs = parse_procs(output.get(..uname_at).unwrap_or(""));
    Ok(Parsed::Linux(Raw { uptime, cpu_total, cpu_idle, cores, mem_total_kb, mem_available_kb, rx, tx, page, procs }))
}

fn parse_uptime(text: &str) -> Result<f64, &'static str> {
    let value = text.lines().next().and_then(|line| line.split_ascii_whitespace().next()).and_then(|word| word.parse::<f64>().ok());
    match value {
        Some(v) if v.is_finite() && v >= 0.0 => Ok(v),
        _ => Err(UNREADABLE),
    }
}

/// The aggregate cpu line as (all ticks, idle ticks) and the number of cores.
fn parse_cpu(text: &str) -> Result<(u64, u64, usize), &'static str> {
    let mut total = None;
    let mut cores = 0usize;
    for line in text.lines().take(MAX_PROCS) {
        let Some(rest) = line.strip_prefix("cpu") else { continue };
        if rest.starts_with(|c: char| c.is_ascii_digit()) {
            cores = cores.saturating_add(1);
        } else if rest.starts_with(char::is_whitespace) && total.is_none() {
            let numbers: Vec<u64> = rest.split_ascii_whitespace().take(8).map(|w| w.parse::<u64>().unwrap_or(0)).collect();
            let all = numbers.iter().fold(0u64, |sum, n| sum.saturating_add(*n));
            let idle = numbers.get(3).copied().unwrap_or(0).saturating_add(numbers.get(4).copied().unwrap_or(0));
            total = Some((all, idle));
        }
    }
    let (all, idle) = total.ok_or(UNREADABLE)?;
    Ok((all, idle, cores.max(1)))
}

/// `MemTotal` and the memory that is free to use, in kilobytes.
fn parse_mem(text: &str) -> Result<(u64, u64), &'static str> {
    let (mut total, mut available, mut free, mut buffers, mut cached) = (None, None, 0u64, 0u64, 0u64);
    for line in text.lines().take(MAX_PROCS) {
        let Some((key, rest)) = line.split_once(':') else { continue };
        let Some(value) = rest.split_ascii_whitespace().next().and_then(|w| w.parse::<u64>().ok()) else { continue };
        match key {
            "MemTotal" => total = Some(value),
            "MemAvailable" => available = Some(value),
            "MemFree" => free = value,
            "Buffers" => buffers = value,
            "Cached" => cached = value,
            _ => {}
        }
    }
    let total = total.filter(|t| *t > 0).ok_or(UNREADABLE)?;
    // Kernels before 3.14 have no MemAvailable: what is free, in buffers and in the cache is the estimate.
    let available = available.unwrap_or_else(|| free.saturating_add(buffers).saturating_add(cached));
    Ok((total, available))
}

/// Bytes received and sent, over every interface except the loopback one.
fn parse_net(text: &str) -> (u64, u64) {
    let (mut rx, mut tx) = (0u64, 0u64);
    for line in text.lines().take(MAX_PROCS) {
        let Some((name, counters)) = line.rsplit_once(':') else { continue };
        if name.trim() == "lo" {
            continue;
        }
        let mut fields = counters.split_ascii_whitespace();
        let received = fields.next().and_then(|w| w.parse::<u64>().ok());
        let sent = fields.nth(7).and_then(|w| w.parse::<u64>().ok());
        if let (Some(r), Some(t)) = (received, sent) {
            rx = rx.saturating_add(r);
            tx = tx.saturating_add(t);
        }
    }
    (rx, tx)
}

fn parse_page(text: &str) -> u64 {
    let size = text.lines().next().and_then(|line| line.trim().parse::<u64>().ok());
    match size {
        Some(s) if s.is_power_of_two() && PAGE_RANGE.contains(&s) => s,
        _ => DEFAULT_PAGE,
    }
}

/// The processes of the region before `@@uname`. A text that does not start with its first marker lost its front, so its
/// first line is a piece of a line and is dropped.
fn parse_procs(region: &str) -> Vec<RawProc> {
    let rest = match region.strip_prefix(BEGIN) {
        Some(rest) => rest,
        // The front was cut: the first line is a piece of a line.
        None => region.split_once('\n').map_or("", |(_, rest)| rest),
    };
    rest.lines().take(MAX_PROCS).filter_map(proc_line).collect()
}

/// One line of the per-process stat file: the pid, the name in round brackets (which may hold spaces and brackets of its
/// own), the state letter and the numbers. Fields are counted after the last bracket.
pub(super) fn proc_line(line: &str) -> Option<RawProc> {
    if line.is_empty() || line.len() > MAX_LINE {
        return None;
    }
    let open = line.find('(')?;
    let close = line.rfind(')')?;
    if close <= open {
        return None;
    }
    let pid = line.get(..open)?.trim().parse::<u32>().ok()?;
    let name = clean(line.get(open + 1..close)?);
    let fields: Vec<&str> = line.get(close + 1..)?.split_ascii_whitespace().take(24).collect();
    let number = |index: usize| fields.get(index).and_then(|w| w.parse::<u64>().ok());
    let user = number(11)?;
    let system = number(12)?;
    let start = number(19)?;
    // A resident page count can be reported negative; it is read wide and cut to zero.
    let pages = fields.get(21).and_then(|w| w.parse::<i128>().ok())?;
    let rss_pages = u64::try_from(pages.max(0)).unwrap_or(u64::MAX);
    Some(RawProc { pid, start, name, ticks: user.saturating_add(system), rss_pages })
}

/// The step from the last reading to this one. A previous reading counts only when time moved forward and no tick total
/// went back (a host that restarted, or a counter that wrapped): else this is a first reading with no rates.
pub(super) fn derive(prev: Option<&Raw>, now: &Raw) -> Snapshot {
    let prev = prev.filter(|p| now.uptime > p.uptime && now.cpu_total >= p.cpu_total);
    let page = now.page;
    let (cpu, net_rx, net_tx, total_ticks) = match prev {
        Some(p) => {
            let secs = now.uptime - p.uptime;
            let total = now.cpu_total - p.cpu_total;
            let idle = now.cpu_idle.saturating_sub(p.cpu_idle);
            let busy = total.saturating_sub(idle);
            let cpu = if total == 0 { 0.0 } else { (busy as f64 * 100.0 / total as f64).clamp(0.0, 100.0) };
            (cpu as f32, rate(p.rx, now.rx, secs), rate(p.tx, now.tx, secs), total)
        }
        None => (0.0, 0, 0, 0),
    };
    let before: HashMap<u32, (u64, u64)> = prev.map_or_else(HashMap::new, |p| p.procs.iter().map(|r| (r.pid, (r.start, r.ticks))).collect());
    let cores = now.cores.max(1);
    let ceiling = 100.0 * cores as f64;
    let procs = now
        .procs
        .iter()
        .map(|r| {
            let cpu = match before.get(&r.pid) {
                Some((start, ticks)) if *start == r.start && total_ticks > 0 => {
                    let used = r.ticks.saturating_sub(*ticks);
                    // Ticks times cores times 100, then over the whole tick difference: 20 of 800 on four cores is
                    // exactly 10.0.
                    (used as f64 * cores as f64 * 100.0 / total_ticks as f64).clamp(0.0, ceiling)
                }
                _ => 0.0,
            };
            Proc { pid: r.pid, name: r.name.clone(), cpu: cpu as f32, mem: r.rss_pages.saturating_mul(page) }
        })
        .collect();
    Snapshot {
        cpu,
        cores,
        mem_used: now.mem_total_kb.saturating_mul(1024).saturating_sub(now.mem_available_kb.saturating_mul(1024)),
        mem_total: now.mem_total_kb.saturating_mul(1024),
        net_rx,
        net_tx,
        uptime: now.uptime as u64,
        procs: busiest(procs, TOP),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn fixture(name: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/monitor").join(name);
        std::fs::read_to_string(path).expect("the fixture exists")
    }

    fn linux(text: &str) -> Raw {
        match parse(text) {
            Ok(Parsed::Linux(raw)) => raw,
            other => panic!("not a Linux reading: {other:?}"),
        }
    }

    #[test]
    fn a_linux_answer_is_read_section_by_section() {
        let raw = linux(&fixture("linux_a.txt"));
        assert_eq!(raw.uptime, 5400.0);
        assert_eq!(raw.cores, 4);
        assert_eq!((raw.cpu_total, raw.cpu_idle), (8000, 6500));
        assert_eq!((raw.mem_total_kb, raw.mem_available_kb), (16_384_000, 10_000_000));
        assert_eq!((raw.rx, raw.tx), (10_500_000, 2_100_000), "the loopback is left out");
        assert_eq!(raw.page, 4096);
        assert_eq!(raw.procs.len(), 4);
    }

    #[test]
    fn a_process_name_with_spaces_and_brackets_is_read_whole() {
        let raw = linux(&fixture("linux_a.txt"));
        let p = raw.procs.iter().find(|p| p.pid == 977).expect("pid 977");
        assert_eq!(p.name, "a (b) c");
        assert_eq!(p.ticks, 30);
        assert_eq!(p.start, 3000);
        assert_eq!(p.rss_pages, 20_000);
    }

    #[test]
    fn cpu_is_the_busy_share_between_two_readings() {
        let a = linux(&fixture("linux_a.txt"));
        let b = linux(&fixture("linux_b.txt"));
        assert_eq!(derive(None, &a).cpu, 0.0, "a first reading has nothing to compare with");
        assert_eq!(derive(Some(&a), &b).cpu, 37.5);
    }

    #[test]
    fn a_process_is_a_share_of_one_core_and_a_new_or_reused_pid_counts_from_zero() {
        let a = linux(&fixture("linux_a.txt"));
        let b = linux(&fixture("linux_b.txt"));
        let snapshot = derive(Some(&a), &b);
        let cpu = |pid: u32| snapshot.procs.iter().find(|p| p.pid == pid).map(|p| p.cpu);
        assert_eq!(cpu(842), Some(200.0));
        assert_eq!(cpu(1), Some(10.0));
        assert_eq!(cpu(977), Some(0.0));
        assert_eq!(cpu(1500), Some(0.0), "a new pid");
        assert_eq!(cpu(7), Some(0.0), "the same pid with another start time");
        assert_eq!(snapshot.procs.iter().map(|p| p.pid).collect::<Vec<_>>(), vec![842, 1, 977, 1500, 7]);
    }
}
