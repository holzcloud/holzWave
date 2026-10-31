//! The text of `/proc/<pid>/status` as data, and the audit of a process table against what the design says.
//!
//! The parser and the audit are plain functions over text and rows, so they run on any machine; only the walker that reads
//! the real `/proc` is Linux code. Tests pull this file in with `#[path = "support/proc.rs"] mod proc;`.
#![allow(dead_code)]

use std::fmt;

/// The capabilities the spawn helper holds: `cap_chown` (bit 0), `cap_kill` (bit 5), `cap_setgid` (bit 6), `cap_setuid` (bit 7).
pub const FOUR: u64 = (1 << 0) | (1 << 5) | (1 << 6) | (1 << 7);

/// The names of the capability bits a current kernel defines, by bit number.
pub const CAP_NAMES: [&str; 41] = [
    "cap_chown",
    "cap_dac_override",
    "cap_dac_read_search",
    "cap_fowner",
    "cap_fsetid",
    "cap_kill",
    "cap_setgid",
    "cap_setuid",
    "cap_setpcap",
    "cap_linux_immutable",
    "cap_net_bind_service",
    "cap_net_broadcast",
    "cap_net_admin",
    "cap_net_raw",
    "cap_ipc_lock",
    "cap_ipc_owner",
    "cap_sys_module",
    "cap_sys_rawio",
    "cap_sys_chroot",
    "cap_sys_ptrace",
    "cap_sys_pacct",
    "cap_sys_admin",
    "cap_sys_boot",
    "cap_sys_nice",
    "cap_sys_resource",
    "cap_sys_time",
    "cap_sys_tty_config",
    "cap_mknod",
    "cap_lease",
    "cap_audit_write",
    "cap_audit_control",
    "cap_setfcap",
    "cap_mac_override",
    "cap_mac_admin",
    "cap_syslog",
    "cap_wake_alarm",
    "cap_block_suspend",
    "cap_audit_read",
    "cap_perfmon",
    "cap_bpf",
    "cap_checkpoint_restore",
];

/// The names of the bits set in `mask`, lowest bit first. A bit past the table is `cap_<n>`.
pub fn cap_names(mask: u64) -> Vec<String> {
    (0..64)
        .filter(|bit| mask & (1u64 << bit) != 0)
        .map(|bit| CAP_NAMES.get(bit).map_or_else(|| format!("cap_{bit}"), |name| (*name).to_owned()))
        .collect()
}

/// Why a status text could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StatusError {
    /// A field the audit needs is not in the text.
    Missing(&'static str),
    /// A field is there and does not have the shape it should.
    Malformed(&'static str),
}

impl fmt::Display for StatusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StatusError::Missing(field) => write!(f, "the status text has no {field} line"),
            StatusError::Malformed(field) => write!(f, "the {field} line of the status text is malformed"),
        }
    }
}

/// What `/proc/<pid>/status` says about one process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    pub name: String,
    pub state: char,
    pub pid: i32,
    pub ppid: i32,
    /// Real, effective, saved and file-system user id.
    pub uid: [u32; 4],
    /// Real, effective, saved and file-system group id.
    pub gid: [u32; 4],
    pub groups: Vec<u32>,
    pub cap_inh: u64,
    pub cap_prm: u64,
    pub cap_eff: u64,
    pub cap_bnd: u64,
    pub cap_amb: u64,
    pub no_new_privs: bool,
}

impl Status {
    /// Whether the process is gone for every purpose of a test: a zombie waits only to be reaped and an exited one is leaving.
    pub fn is_dead(&self) -> bool {
        matches!(self.state, 'Z' | 'X')
    }

    /// Whether any of the four user ids is `uid`.
    pub fn has_uid(&self, uid: u32) -> bool {
        self.uid.contains(&uid)
    }

    /// Whether any of the four user ids is in `low..=high`.
    pub fn in_range(&self, low: u32, high: u32) -> bool {
        self.uid.iter().any(|id| (low..=high).contains(id))
    }

    /// One line for the SUMMARY: pid, parent, name, ids, the four sets and `NoNewPrivs`.
    pub fn line(&self) -> String {
        format!(
            "pid {} ppid {} {} state {} uid {:?} gid {:?} groups {:?} inh {:016x} prm {:016x} eff {:016x} amb {:016x} bnd {:016x} nnp {}",
            self.pid,
            self.ppid,
            self.name,
            self.state,
            self.uid,
            self.gid,
            self.groups,
            self.cap_inh,
            self.cap_prm,
            self.cap_eff,
            self.cap_amb,
            self.cap_bnd,
            u8::from(self.no_new_privs)
        )
    }
}

fn field<'a>(text: &'a str, name: &'static str) -> Result<&'a str, StatusError> {
    for line in text.lines() {
        if let Some((key, value)) = line.split_once(':')
            && key == name
        {
            return Ok(value.trim());
        }
    }
    Err(StatusError::Missing(name))
}

fn four(text: &str, name: &'static str) -> Result<[u32; 4], StatusError> {
    let numbers: Vec<u32> = field(text, name)?
        .split_whitespace()
        .map(str::parse)
        .collect::<Result<_, _>>()
        .map_err(|_| StatusError::Malformed(name))?;
    numbers.try_into().map_err(|_| StatusError::Malformed(name))
}

fn mask(text: &str, name: &'static str) -> Result<u64, StatusError> {
    let value = field(text, name)?;
    if value.len() != 16 {
        return Err(StatusError::Malformed(name));
    }
    u64::from_str_radix(value, 16).map_err(|_| StatusError::Malformed(name))
}

/// Reads the text of `/proc/<pid>/status`.
pub fn parse_status(text: &str) -> Result<Status, StatusError> {
    let state = field(text, "State")?.chars().next().ok_or(StatusError::Malformed("State"))?;
    let number = |name: &'static str| -> Result<i32, StatusError> { field(text, name)?.parse().map_err(|_| StatusError::Malformed(name)) };
    let groups = field(text, "Groups")?
        .split_whitespace()
        .map(str::parse)
        .collect::<Result<Vec<u32>, _>>()
        .map_err(|_| StatusError::Malformed("Groups"))?;
    let no_new_privs = match field(text, "NoNewPrivs")? {
        "0" => false,
        "1" => true,
        _ => return Err(StatusError::Malformed("NoNewPrivs")),
    };
    Ok(Status {
        name: field(text, "Name")?.to_owned(),
        state,
        pid: number("Pid")?,
        ppid: number("PPid")?,
        uid: four(text, "Uid")?,
        gid: four(text, "Gid")?,
        groups,
        cap_inh: mask(text, "CapInh")?,
        cap_prm: mask(text, "CapPrm")?,
        cap_eff: mask(text, "CapEff")?,
        cap_bnd: mask(text, "CapBnd")?,
        cap_amb: mask(text, "CapAmb")?,
        no_new_privs,
    })
}

/// Every process of the machine, read from `/proc`. A process that ends while the table is read is left out.
#[cfg(target_os = "linux")]
pub fn process_table() -> Vec<Status> {
    let mut rows = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else { return rows };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(format!("/proc/{name}/status")) else { continue };
        if let Ok(status) = parse_status(&text) {
            rows.push(status);
        }
    }
    rows.sort_by_key(|row| row.pid);
    rows
}

/// The pids below `root` (children, their children, and so on), not `root` itself.
pub fn descendants(rows: &[Status], root: i32) -> Vec<i32> {
    let mut found = Vec::new();
    let mut frontier = vec![root];
    while let Some(parent) = frontier.pop() {
        for row in rows.iter().filter(|row| row.ppid == parent && row.pid != parent) {
            if !found.contains(&row.pid) {
                found.push(row.pid);
                frontier.push(row.pid);
            }
        }
    }
    found.sort_unstable();
    found
}

/// The living processes that have `uid` in any of their user ids.
pub fn of_uid(rows: &[Status], uid: u32) -> Vec<&Status> {
    rows.iter().filter(|row| !row.is_dead() && row.has_uid(uid)).collect()
}

// ---- the audit --------------------------------------------------------------------------------------------------------

/// What the table is expected to hold.
#[derive(Clone, Copy, Debug)]
pub struct Expect {
    /// The process of the supervisor (the test itself).
    pub supervisor: i32,
    /// The user id of the supervisor.
    pub own_uid: u32,
    /// The process of the spawn helper, when it runs.
    pub helper: Option<i32>,
    /// The range of user ids the people get.
    pub range: (u32, u32),
}

/// One thing the table does that the design says it must not, with the process it was found in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Violation {
    /// A process has user id 0 in one of its four user id fields.
    Root { pid: i32 },
    /// The row of the supervisor is not in the table.
    NoSupervisor,
    /// The supervisor does not run as its own user in all four fields.
    SupervisorIds { pid: i32 },
    /// The supervisor holds a capability in the named set.
    SupervisorCapability { pid: i32, set: &'static str },
    /// The supervisor has `NoNewPrivs`, which makes the kernel ignore the file capabilities of the helper.
    SupervisorNoNewPrivs { pid: i32 },
    /// The helper is not in the table.
    NoHelper,
    /// The helper does not run as the supervisor's user in all four fields.
    HelperIds { pid: i32 },
    /// The helper's permitted set is not exactly the four capabilities.
    HelperPermitted { pid: i32, found: u64 },
    /// The helper's effective set is something other than empty (at rest) or the four.
    HelperEffective { pid: i32, found: u64 },
    /// The helper has an inheritable or an ambient capability.
    HelperInheritedOrAmbient { pid: i32 },
    /// The helper has `NoNewPrivs`.
    HelperNoNewPrivs { pid: i32 },
    /// A process of a person has user or group ids that are not all the person's own.
    PersonIds { pid: i32 },
    /// A process of a person has a supplementary group.
    PersonGroups { pid: i32 },
    /// A process of a person holds a capability in the named set.
    PersonCapability { pid: i32, set: &'static str },
    /// A process of a person does not have `NoNewPrivs`.
    PersonNoNewPrivs { pid: i32 },
    /// A process that is neither the helper nor a person's holds a capability.
    StrayCapability { pid: i32, set: &'static str },
}

fn sets(row: &Status) -> [(&'static str, u64); 4] {
    [("inheritable", row.cap_inh), ("permitted", row.cap_prm), ("effective", row.cap_eff), ("ambient", row.cap_amb)]
}

/// Checks a table against the rules of the design for the process tree. The result is empty when the table is clean, and
/// otherwise names every violation once per process and kind.
pub fn audit(rows: &[Status], expect: &Expect) -> Vec<Violation> {
    let mut found = Vec::new();
    let (low, high) = expect.range;
    let live: Vec<&Status> = rows.iter().filter(|row| !row.is_dead()).collect();

    for row in &live {
        if row.uid.contains(&0) {
            found.push(Violation::Root { pid: row.pid });
        }
    }

    match live.iter().find(|row| row.pid == expect.supervisor) {
        None => found.push(Violation::NoSupervisor),
        Some(row) => {
            if row.uid != [expect.own_uid; 4] {
                found.push(Violation::SupervisorIds { pid: row.pid });
            }
            for (set, value) in sets(row) {
                if value != 0 {
                    found.push(Violation::SupervisorCapability { pid: row.pid, set });
                }
            }
            if row.no_new_privs {
                found.push(Violation::SupervisorNoNewPrivs { pid: row.pid });
            }
        }
    }

    match expect.helper {
        None => {}
        Some(pid) => match live.iter().find(|row| row.pid == pid) {
            None => found.push(Violation::NoHelper),
            Some(row) => {
                if row.uid != [expect.own_uid; 4] {
                    found.push(Violation::HelperIds { pid });
                }
                if row.cap_prm != FOUR {
                    found.push(Violation::HelperPermitted { pid, found: row.cap_prm });
                }
                if row.cap_eff != 0 && row.cap_eff != FOUR {
                    found.push(Violation::HelperEffective { pid, found: row.cap_eff });
                }
                if row.cap_inh != 0 || row.cap_amb != 0 {
                    found.push(Violation::HelperInheritedOrAmbient { pid });
                }
                if row.no_new_privs {
                    found.push(Violation::HelperNoNewPrivs { pid });
                }
            }
        },
    }

    for row in &live {
        if Some(row.pid) == expect.helper || row.pid == expect.supervisor {
            continue;
        }
        if row.in_range(low, high) {
            let own = row.uid[0];
            if row.uid != [own; 4] || row.gid != [own; 4] {
                found.push(Violation::PersonIds { pid: row.pid });
            }
            if !row.groups.is_empty() {
                found.push(Violation::PersonGroups { pid: row.pid });
            }
            for (set, value) in sets(row) {
                if value != 0 {
                    found.push(Violation::PersonCapability { pid: row.pid, set });
                }
            }
            if !row.no_new_privs {
                found.push(Violation::PersonNoNewPrivs { pid: row.pid });
            }
        } else {
            for (set, value) in [("permitted", row.cap_prm), ("effective", row.cap_eff)] {
                if value != 0 {
                    found.push(Violation::StrayCapability { pid: row.pid, set });
                }
            }
        }
    }
    found
}

// ---- descriptors ------------------------------------------------------------------------------------------------------

/// The entries of the text of `ls -l /proc/<pid>/fd` that a person's code must not hold: a socket, an `anon_inode`, and
/// any descriptor of 3 or above that points neither to the pane's pseudo-terminal nor to a path below `home`. The problems
/// come back as the entry's line, in the order of the listing. Lines that are not entries (the `total` line) are ignored.
pub fn fd_listing_problems(listing: &str, home: &str) -> Vec<String> {
    let below_home = format!("{}/", home.trim_end_matches('/'));
    let mut problems = Vec::new();
    for line in listing.lines() {
        let Some((left, target)) = line.split_once(" -> ") else { continue };
        let target = target.trim();
        let Some(number) = left.split_whitespace().last().and_then(|word| word.parse::<u32>().ok()) else { continue };
        let allowed_target = target.starts_with("/dev/pts/")
            || target == "/dev/tty"
            || target == "/dev/ptmx"
            || target == "/dev/null"
            || target.starts_with(&below_home);
        if target.starts_with("socket:") || target.starts_with("anon_inode:") || (number >= 3 && !allowed_target) {
            problems.push(line.trim().to_owned());
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(clippy::too_many_arguments)]
    fn sample(name: &str, pid: i32, ppid: i32, uid: u32, gid: u32, groups: &str, caps: [&str; 5], nnp: u8) -> String {
        let [inh, prm, eff, bnd, amb] = caps;
        format!(
            "Name:\t{name}\nUmask:\t0022\nState:\tS (sleeping)\nTgid:\t{pid}\nNgid:\t0\nPid:\t{pid}\nPPid:\t{ppid}\nTracerPid:\t0\n\
             Uid:\t{uid}\t{uid}\t{uid}\t{uid}\nGid:\t{gid}\t{gid}\t{gid}\t{gid}\nFDSize:\t64\nGroups:\t{groups}\nNStgid:\t{pid}\n\
             CapInh:\t{inh}\nCapPrm:\t{prm}\nCapEff:\t{eff}\nCapBnd:\t{bnd}\nCapAmb:\t{amb}\nNoNewPrivs:\t{nnp}\nSeccomp:\t0\n"
        )
    }

    const NONE: &str = "0000000000000000";
    const FOUR_TEXT: &str = "00000000000000e1";

    fn root_text() -> String {
        sample("init", 1, 0, 0, 0, "0", [NONE, "000001ffffffffff", "000001ffffffffff", "000001ffffffffff", NONE], 0)
    }
    fn supervisor_text() -> String {
        sample("isolation", 40, 1, 10001, 10001, "", [NONE, NONE, NONE, FOUR_TEXT, NONE], 0)
    }
    fn helper_text() -> String {
        sample("holzwave-spawn-", 41, 40, 10001, 10001, "", [NONE, FOUR_TEXT, NONE, FOUR_TEXT, NONE], 0)
    }
    fn worker_text(pid: i32, ppid: i32, uid: u32) -> String {
        sample("holzwave-server", pid, ppid, uid, uid, "", [NONE, NONE, NONE, FOUR_TEXT, NONE], 1)
    }

    #[test]
    fn a_root_process_is_read_with_its_groups_and_full_sets() {
        let status = parse_status(&root_text()).expect("a status text");
        assert_eq!((status.pid, status.ppid, status.name.as_str()), (1, 0, "init"));
        assert_eq!(status.uid, [0; 4]);
        assert_eq!(status.gid, [0; 4]);
        assert_eq!(status.groups, vec![0]);
        assert_eq!(status.cap_prm, 0x1ff_ffff_ffff);
        assert!(!status.no_new_privs);
    }

    #[test]
    fn the_supervisor_has_empty_sets_and_no_nonewprivs() {
        let status = parse_status(&supervisor_text()).expect("a status text");
        assert_eq!(status.uid, [10001; 4]);
        assert_eq!((status.cap_inh, status.cap_prm, status.cap_eff, status.cap_amb), (0, 0, 0, 0));
        assert_eq!(status.cap_bnd, FOUR, "the container's bounding set");
        assert!(!status.no_new_privs);
        assert!(status.groups.is_empty());
    }

    #[test]
    fn the_helper_holds_four_bits_and_a_worker_holds_none() {
        let helper = parse_status(&helper_text()).expect("a status text");
        assert_eq!(helper.cap_prm, FOUR);
        assert_eq!(cap_names(helper.cap_prm), ["cap_chown", "cap_kill", "cap_setgid", "cap_setuid"]);
        let worker = parse_status(&worker_text(50, 41, 20000)).expect("a status text");
        assert_eq!((worker.cap_prm, worker.cap_eff), (0, 0));
        assert!(worker.no_new_privs);
        assert_eq!(worker.uid, [20000; 4]);
    }

    #[test]
    fn the_four_capabilities_have_their_kernel_bit_numbers() {
        assert_eq!(CAP_NAMES[0], "cap_chown");
        assert_eq!(CAP_NAMES[5], "cap_kill");
        assert_eq!(CAP_NAMES[6], "cap_setgid");
        assert_eq!(CAP_NAMES[7], "cap_setuid");
        assert_eq!(FOUR, 0xe1);
        assert_eq!(cap_names(1 << 7), ["cap_setuid"]);
        assert_eq!(cap_names(1 << 6), ["cap_setgid"]);
        assert_eq!(cap_names(1 << 0), ["cap_chown"]);
        assert_eq!(cap_names(1 << 5), ["cap_kill"]);
        assert_eq!(cap_names(0), Vec::<String>::new());
        assert_eq!(cap_names(1 << 60), ["cap_60"], "a bit past the table is still named");
        assert_eq!(CAP_NAMES.len(), 41);
    }

    #[test]
    fn a_missing_or_malformed_field_is_a_named_error() {
        const NAMES: [&str; 13] = ["Name", "State", "Pid", "PPid", "Uid", "Gid", "Groups", "CapInh", "CapPrm", "CapEff", "CapBnd", "CapAmb", "NoNewPrivs"];
        for name in NAMES {
            let text: String = supervisor_text().lines().filter(|line| !line.starts_with(&format!("{name}:"))).map(|l| format!("{l}\n")).collect();
            assert_eq!(parse_status(&text), Err(StatusError::Missing(name)), "{name}");
        }
        let bad_uid = supervisor_text().replace("Uid:\t10001\t10001\t10001\t10001", "Uid:\t10001\t10001");
        assert_eq!(parse_status(&bad_uid), Err(StatusError::Malformed("Uid")));
        let bad_cap = supervisor_text().replace("CapPrm:\t0000000000000000", "CapPrm:\tzzzzzzzzzzzzzzzz");
        assert_eq!(parse_status(&bad_cap), Err(StatusError::Malformed("CapPrm")));
        let short_cap = supervisor_text().replace("CapEff:\t0000000000000000", "CapEff:\t0");
        assert_eq!(parse_status(&short_cap), Err(StatusError::Malformed("CapEff")));
        let bad_flag = supervisor_text().replace("NoNewPrivs:\t0", "NoNewPrivs:\t7");
        assert_eq!(parse_status(&bad_flag), Err(StatusError::Malformed("NoNewPrivs")));
        let bad_group = supervisor_text().replace("Groups:\t", "Groups:\tx y");
        assert_eq!(parse_status(&bad_group), Err(StatusError::Malformed("Groups")));
        assert_eq!(parse_status(""), Err(StatusError::Missing("State")));
        assert!(StatusError::Missing("Uid").to_string().contains("Uid"));
    }

    fn table(texts: &[String]) -> Vec<Status> {
        texts.iter().map(|text| parse_status(text).expect("a status text")).collect()
    }

    fn expect() -> Expect {
        Expect { supervisor: 40, own_uid: 10001, helper: Some(41), range: (20000, 29999) }
    }

    fn clean() -> Vec<String> {
        vec![
            sample("docker-init", 1, 0, 10001, 10001, "", [NONE, NONE, NONE, FOUR_TEXT, NONE], 0),
            supervisor_text(),
            helper_text(),
            worker_text(50, 41, 20000),
            sample("bash", 51, 50, 20000, 20000, "", [NONE, NONE, NONE, FOUR_TEXT, NONE], 1),
            sample("sleep", 52, 51, 20000, 20000, "", [NONE, NONE, NONE, FOUR_TEXT, NONE], 1),
            worker_text(60, 41, 20001),
        ]
    }

    #[test]
    fn the_clean_table_has_no_violation() {
        assert_eq!(audit(&table(&clean()), &expect()), vec![]);
    }

    #[test]
    fn a_uid_zero_row_is_named() {
        let mut texts = clean();
        texts.push(sample("evil", 70, 1, 0, 0, "", [NONE, NONE, NONE, FOUR_TEXT, NONE], 0));
        assert_eq!(audit(&table(&texts), &expect()), vec![Violation::Root { pid: 70 }]);
    }

    #[test]
    fn a_worker_with_an_effective_capability_is_named() {
        let mut texts = clean();
        texts[3] = sample("holzwave-server", 50, 41, 20000, 20000, "", [NONE, NONE, FOUR_TEXT, FOUR_TEXT, NONE], 1);
        assert_eq!(audit(&table(&texts), &expect()), vec![Violation::PersonCapability { pid: 50, set: "effective" }]);
    }

    #[test]
    fn a_worker_without_nonewprivs_is_named() {
        let mut texts = clean();
        texts[6] = sample("holzwave-server", 60, 41, 20001, 20001, "", [NONE, NONE, NONE, FOUR_TEXT, NONE], 0);
        assert_eq!(audit(&table(&texts), &expect()), vec![Violation::PersonNoNewPrivs { pid: 60 }]);
    }

    #[test]
    fn a_helper_without_capabilities_is_named() {
        let mut texts = clean();
        texts[2] = sample("holzwave-spawn-", 41, 40, 10001, 10001, "", [NONE, NONE, NONE, FOUR_TEXT, NONE], 0);
        assert_eq!(audit(&table(&texts), &expect()), vec![Violation::HelperPermitted { pid: 41, found: 0 }]);
    }

    #[test]
    fn a_helper_that_raised_its_effective_set_is_clean_and_one_with_more_is_not() {
        let mut texts = clean();
        texts[2] = sample("holzwave-spawn-", 41, 40, 10001, 10001, "", [NONE, FOUR_TEXT, FOUR_TEXT, FOUR_TEXT, NONE], 0);
        assert_eq!(audit(&table(&texts), &expect()), vec![]);
        texts[2] = sample("holzwave-spawn-", 41, 40, 10001, 10001, "", [NONE, FOUR_TEXT, "0000000000000021", FOUR_TEXT, NONE], 0);
        assert_eq!(audit(&table(&texts), &expect()), vec![Violation::HelperEffective { pid: 41, found: 0x21 }]);
    }

    #[test]
    fn a_supervisor_with_nonewprivs_is_named() {
        let mut texts = clean();
        texts[1] = sample("isolation", 40, 1, 10001, 10001, "", [NONE, NONE, NONE, FOUR_TEXT, NONE], 1);
        assert_eq!(audit(&table(&texts), &expect()), vec![Violation::SupervisorNoNewPrivs { pid: 40 }]);
    }

    #[test]
    fn a_supervisor_with_a_capability_is_named() {
        let mut texts = clean();
        texts[1] = sample("isolation", 40, 1, 10001, 10001, "", [NONE, FOUR_TEXT, NONE, FOUR_TEXT, NONE], 0);
        assert_eq!(audit(&table(&texts), &expect()), vec![Violation::SupervisorCapability { pid: 40, set: "permitted" }]);
    }

    #[test]
    fn a_worker_with_a_supplementary_group_is_named() {
        let mut texts = clean();
        texts[3] = sample("holzwave-server", 50, 41, 20000, 20000, "20000", [NONE, NONE, NONE, FOUR_TEXT, NONE], 1);
        assert_eq!(audit(&table(&texts), &expect()), vec![Violation::PersonGroups { pid: 50 }]);
    }

    #[test]
    fn a_shell_with_a_foreign_group_or_a_mixed_user_id_is_named() {
        let mut texts = clean();
        texts[4] = sample("bash", 51, 50, 20000, 10001, "", [NONE, NONE, NONE, FOUR_TEXT, NONE], 1);
        assert_eq!(audit(&table(&texts), &expect()), vec![Violation::PersonIds { pid: 51 }]);
        let mixed = Status { uid: [20000, 20000, 10001, 20000], ..parse_status(&clean()[5]).expect("a status text") };
        let mut rows = table(&clean());
        rows[5] = mixed;
        assert_eq!(audit(&rows, &expect()), vec![Violation::PersonIds { pid: 52 }]);
    }

    #[test]
    fn a_stray_capability_in_an_ordinary_process_is_named() {
        let mut texts = clean();
        texts[0] = sample("docker-init", 1, 0, 10001, 10001, "", [NONE, "0000000000000001", NONE, FOUR_TEXT, NONE], 0);
        assert_eq!(audit(&table(&texts), &expect()), vec![Violation::StrayCapability { pid: 1, set: "permitted" }]);
    }

    #[test]
    fn a_missing_supervisor_or_helper_is_named_and_a_zombie_is_not_a_process() {
        let rows = table(&clean()[3..]);
        assert_eq!(audit(&rows, &expect()), vec![Violation::NoSupervisor, Violation::NoHelper]);
        let mut rows = table(&clean());
        rows[5].state = 'Z';
        rows[5].uid = [0; 4];
        assert_eq!(audit(&rows, &expect()), vec![], "a zombie is not audited");
    }

    #[test]
    fn descendants_follow_the_parent_ids() {
        let rows = table(&clean());
        assert_eq!(descendants(&rows, 50), vec![51, 52]);
        assert_eq!(descendants(&rows, 41), vec![50, 51, 52, 60]);
        assert_eq!(descendants(&rows, 52), Vec::<i32>::new());
        assert_eq!(of_uid(&rows, 20000).len(), 3);
        assert_eq!(of_uid(&rows, 20001).len(), 1);
    }

    const HOME: &str = "/data/homes/20000";

    #[test]
    fn a_clean_listing_has_no_problem() {
        let listing = "total 0\n\
            lrwx------ 1 20000 20000 64 Oct 31 10:00 0 -> /dev/pts/0\n\
            l-wx------ 1 20000 20000 64 Oct 31 10:00 1 -> /data/homes/20000/.tmp/hw-1.out\n\
            lrwx------ 1 20000 20000 64 Oct 31 10:00 2 -> /dev/pts/0\n\
            lrwx------ 1 20000 20000 64 Oct 31 10:00 10 -> /dev/tty\n\
            lrwx------ 1 20000 20000 64 Oct 31 10:00 11 -> /dev/pts/0\n";
        assert_eq!(fd_listing_problems(listing, HOME), Vec::<String>::new());
    }

    #[test]
    fn a_socket_on_descriptor_three_is_a_problem() {
        let listing = "lrwx------ 1 20000 20000 64 Oct 31 10:00 0 -> /dev/pts/0\n\
            lrwx------ 1 20000 20000 64 Oct 31 10:00 3 -> socket:[12345]\n";
        let problems = fd_listing_problems(listing, HOME);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("socket:[12345]"));
    }

    #[test]
    fn an_anon_inode_is_a_problem_on_any_descriptor() {
        let listing = "lrwx------ 1 20000 20000 64 Oct 31 10:00 5 -> anon_inode:[eventpoll]\n\
            lrwx------ 1 20000 20000 64 Oct 31 10:00 0 -> anon_inode:[pidfd]\n";
        assert_eq!(fd_listing_problems(listing, HOME).len(), 2);
    }

    #[test]
    fn a_descriptor_four_outside_the_home_is_a_problem_and_inside_is_not() {
        let outside = "lr-x------ 1 20000 20000 64 Oct 31 10:00 4 -> /data/server/server.sqlite3\n";
        assert_eq!(fd_listing_problems(outside, HOME).len(), 1);
        let other_home = "lr-x------ 1 20000 20000 64 Oct 31 10:00 4 -> /data/homes/20001/secret.txt\n";
        assert_eq!(fd_listing_problems(other_home, HOME).len(), 1, "another person's home is not this home");
        let sibling = "lr-x------ 1 20000 20000 64 Oct 31 10:00 4 -> /data/homes/200000/x\n";
        assert_eq!(fd_listing_problems(sibling, HOME).len(), 1, "a longer name is not below the home");
        let inside = "lr-x------ 1 20000 20000 64 Oct 31 10:00 4 -> /data/homes/20000/notes.txt\n";
        assert_eq!(fd_listing_problems(inside, HOME), Vec::<String>::new());
        let low = "lr-x------ 1 20000 20000 64 Oct 31 10:00 1 -> /var/log/x\n";
        assert_eq!(fd_listing_problems(low, HOME), Vec::<String>::new(), "descriptors 0 to 2 are the caller's business");
    }
}
