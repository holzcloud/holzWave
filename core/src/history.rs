//! The command history: what was typed in the shells, kept in SQLite, found again with a fuzzy search.
//!
//! The shell tells the app each command through `wsh` (see the `ipc` crate): the app
//! calls [`History::add`]. A command is kept only if it passes [`worth_keeping`], which drops what a person would not
//! want written down: a line that starts with a space (the shells' own "do not remember" convention), anything that looks
//! like it holds a password or a token, and anything absurdly long. That filter is best effort, not a guarantee, and it is
//! said so in the docs.
//!
//! The file is private to the user (mode 0600), uses `secure_delete` so a deleted command is overwritten, and is bounded
//! ([`MAX_ROWS`]). Search runs over the distinct commands in memory with `nucleo-matcher` (the matcher of Helix; fzf's
//! scoring, smart case), ranked by the match, then by how recently and how often the command was used.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use rusqlite::{Connection, params};
use serde::Serialize;

use crate::{Error, Result};

/// The longest command that is kept, in bytes.
pub const MAX_CMD: usize = 4096;
/// The most rows the file holds; the oldest tenth goes when it is passed.
pub const MAX_ROWS: i64 = 100_000;
/// The most hits a search returns.
pub const MAX_HITS: usize = 200;

fn err(e: impl std::fmt::Display) -> Error {
    Error::Io(std::io::Error::other(format!("history: {e}")))
}

/// Words that make a command look like it holds a secret.
const SECRET_WORDS: [&str; 10] =
    ["password", "passwd", "secret", "token", "api_key", "api-key", "apikey", "authorization", "bearer ", "sshpass"];

/// Whether a command may be written down.
pub fn worth_keeping(cmd: &str) -> bool {
    if cmd.trim().is_empty() || cmd.len() > MAX_CMD || cmd.starts_with(' ') || cmd.chars().any(|c| c.is_control() && c != '\t') {
        return false;
    }
    let lower = cmd.to_lowercase();
    !SECRET_WORDS.iter().any(|w| lower.contains(w))
}

/// One distinct command with how it was last used.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Distinct {
    cmd: String,
    /// Seconds since the epoch of the last use.
    last: i64,
    count: u32,
    cwd: String,
    host: String,
    exit: Option<i32>,
}

/// A search result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Hit {
    pub cmd: String,
    pub cwd: String,
    pub host: String,
    pub last: i64,
    pub count: u32,
    pub exit: Option<i32>,
    /// The characters of `cmd` that matched, as indices into its chars, for highlighting. Empty for an empty query.
    pub marks: Vec<u32>,
}

pub struct History {
    conn: Mutex<Connection>,
    /// The distinct commands, keyed by host and command.
    cache: Mutex<HashMap<(String, String), Distinct>>,
    adds: Mutex<u32>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl History {
    /// Open (or make) the history file. A file that cannot be opened is an error: the caller runs without history.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(path).map_err(err)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
        Self::with(conn)
    }

    /// A history that lives in memory only (tests, and a start where the file cannot be used).
    pub fn in_memory() -> Result<Self> {
        Self::with(Connection::open_in_memory().map_err(err)?)
    }

    fn with(conn: Connection) -> Result<Self> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL; PRAGMA secure_delete = ON;
             CREATE TABLE IF NOT EXISTS history (
                 id INTEGER PRIMARY KEY, ts INTEGER NOT NULL, cmd TEXT NOT NULL,
                 cwd TEXT NOT NULL, host TEXT NOT NULL, exit INTEGER);
             CREATE INDEX IF NOT EXISTS history_cmd ON history (host, cmd);",
        )
        .map_err(err)?;
        let cache = Self::load_cache(&conn)?;
        Ok(Self { conn: Mutex::new(conn), cache: Mutex::new(cache), adds: Mutex::new(0) })
    }

    /// The distinct commands of the file, with their last use.
    fn load_cache(conn: &Connection) -> Result<HashMap<(String, String), Distinct>> {
        let mut cache = HashMap::new();
        {
            let mut stmt = conn
                .prepare(
                    "SELECT h.host, h.cmd, MAX(h.ts), COUNT(*), \
                            (SELECT cwd FROM history x WHERE x.host = h.host AND x.cmd = h.cmd ORDER BY ts DESC, id DESC LIMIT 1), \
                            (SELECT exit FROM history x WHERE x.host = h.host AND x.cmd = h.cmd ORDER BY ts DESC, id DESC LIMIT 1) \
                     FROM history h GROUP BY h.host, h.cmd",
                )
                .map_err(err)?;
            let rows = stmt
                .query_map([], |r| {
                    Ok(Distinct {
                        host: r.get(0)?,
                        cmd: r.get(1)?,
                        last: r.get(2)?,
                        count: r.get::<_, i64>(3)?.clamp(0, i64::from(u32::MAX)) as u32,
                        cwd: r.get(4)?,
                        exit: r.get(5)?,
                    })
                })
                .map_err(err)?;
            for d in rows.flatten() {
                cache.insert((d.host.clone(), d.cmd.clone()), d);
            }
        }
        Ok(cache)
    }

    /// Write a command down. `false` when it is not worth keeping ([`worth_keeping`]) and nothing was written.
    pub fn add(&self, cmd: &str, cwd: &str, host: &str, exit: Option<i32>, ts: i64) -> Result<bool> {
        let cmd = cmd.trim_end_matches(['\n', '\r']);
        if !worth_keeping(cmd) || host.is_empty() || host.len() > 128 || cwd.len() > 4096 || cwd.chars().any(char::is_control) {
            return Ok(false);
        }
        {
            let conn = lock(&self.conn);
            conn.execute("INSERT INTO history (ts, cmd, cwd, host, exit) VALUES (?1, ?2, ?3, ?4, ?5)", params![ts, cmd, cwd, host, exit])
                .map_err(err)?;
        }
        lock(&self.cache)
            .entry((host.to_owned(), cmd.to_owned()))
            .and_modify(|d| {
                d.last = d.last.max(ts);
                d.count = d.count.saturating_add(1);
                d.cwd = cwd.to_owned();
                d.exit = exit;
            })
            .or_insert_with(|| Distinct { cmd: cmd.to_owned(), last: ts, count: 1, cwd: cwd.to_owned(), host: host.to_owned(), exit });
        let mut adds = lock(&self.adds);
        *adds += 1;
        if *adds >= 500 {
            *adds = 0;
            drop(adds);
            self.prune()?;
        }
        Ok(true)
    }

    /// Keep the file bounded: past [`MAX_ROWS`] the oldest tenth goes.
    fn prune(&self) -> Result<()> {
        self.prune_over(MAX_ROWS)
    }

    fn prune_over(&self, max: i64) -> Result<()> {
        let conn = lock(&self.conn);
        let rows: i64 = conn.query_row("SELECT COUNT(*) FROM history", [], |r| r.get(0)).map_err(err)?;
        if rows > max {
            conn.execute("DELETE FROM history WHERE id IN (SELECT id FROM history ORDER BY ts, id LIMIT ?1)", params![max / 10])
                .map_err(err)?;
            *lock(&self.cache) = Self::load_cache(&conn)?;
        }
        Ok(())
    }

    /// How many commands are written down.
    pub fn count(&self) -> Result<i64> {
        lock(&self.conn).query_row("SELECT COUNT(*) FROM history", [], |r| r.get(0)).map_err(err)
    }

    /// Find commands. An empty `query` lists the most recent. `host` restricts to one machine (`None`: all).
    pub fn search(&self, query: &str, host: Option<&str>, limit: usize) -> Vec<Hit> {
        let limit = limit.clamp(1, MAX_HITS);
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
        let cache = lock(&self.cache);
        let pool = cache.values().filter(|d| host.is_none_or(|h| d.host == h));
        let query = query.trim();
        let mut scored: Vec<(u64, &Distinct, Vec<u32>)> = Vec::new();
        if query.is_empty() {
            scored.extend(pool.map(|d| (d.last.max(0) as u64, d, Vec::new())));
        } else {
            let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
            let mut matcher = Matcher::new(Config::DEFAULT);
            let mut buf = Vec::new();
            for d in pool {
                let hay = Utf32Str::new(&d.cmd, &mut buf);
                let mut marks = Vec::new();
                if let Some(score) = pattern.indices(hay, &mut matcher, &mut marks) {
                    marks.sort_unstable();
                    marks.dedup();
                    let age_days = ((now - d.last).max(0) / 86_400) as u64;
                    // The match decides; use and recency order what matches about equally.
                    let rank = u64::from(score) * 4 + 40u64.saturating_sub(age_days) + u64::from(d.count.min(10)) * 2;
                    scored.push((rank, d, marks));
                }
            }
        }
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.last.cmp(&a.1.last)).then(a.1.cmd.cmp(&b.1.cmd)));
        scored.truncate(limit);
        scored
            .into_iter()
            .map(|(_, d, marks)| Hit {
                cmd: d.cmd.clone(),
                cwd: d.cwd.clone(),
                host: d.host.clone(),
                last: d.last,
                count: d.count,
                exit: d.exit,
                marks,
            })
            .collect()
    }

    /// Forget every use of `cmd` (on every host). The number of rows removed.
    pub fn delete(&self, cmd: &str) -> Result<usize> {
        let n = lock(&self.conn).execute("DELETE FROM history WHERE cmd = ?1", params![cmd]).map_err(err)?;
        lock(&self.cache).retain(|(_, c), _| c != cmd);
        Ok(n)
    }

    /// Forget everything.
    pub fn clear(&self) -> Result<()> {
        let conn = lock(&self.conn);
        conn.execute("DELETE FROM history", []).map_err(err)?;
        // Overwrite the freed pages, so cleared commands are not left in the file.
        let _ = conn.execute_batch("VACUUM");
        lock(&self.cache).clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h() -> History {
        History::in_memory().unwrap()
    }

    fn add(h: &History, cmd: &str, ts: i64) {
        h.add(cmd, "/home/holz", "local", Some(0), ts).unwrap();
    }

    #[test]
    fn secrets_spaces_and_junk_are_not_written_down() {
        let h = h();
        for bad in [" ls", "", "   ", "curl -H 'Authorization: x'", "export API_KEY=abc", "mysql --password=x", "echo $TOKEN", "a\x1bb", "sshpass -p x ssh h"] {
            assert!(!h.add(bad, "/", "local", None, 1).unwrap(), "{bad:?}");
        }
        assert!(!h.add(&"x".repeat(MAX_CMD + 1), "/", "local", None, 1).unwrap());
        assert!(!h.add("ls", "/", "", None, 1).unwrap(), "no host");
        assert_eq!(h.count().unwrap(), 0);
        assert!(h.add("cargo test\n", "/", "local", Some(0), 1).unwrap(), "a trailing newline is not part of the command");
        assert_eq!(h.search("", None, 10)[0].cmd, "cargo test");
    }

    #[test]
    fn a_fuzzy_query_finds_scattered_letters_and_marks_them() {
        let h = h();
        add(&h, "kubectl get pods -n kube-system", 100);
        add(&h, "talosctl dashboard", 200);
        add(&h, "git commit -m 'fix'", 300);
        let hits = h.search("kgp", None, 10);
        assert_eq!(hits[0].cmd, "kubectl get pods -n kube-system");
        assert_eq!(hits[0].marks, vec![0, 8, 12], "k, g and p of the words");
        assert!(h.search("zzzz", None, 10).is_empty());
        let smart = h.search("Git", None, 10);
        assert!(smart.is_empty(), "an upper case letter makes the search case sensitive");
        assert_eq!(h.search("git", None, 10).len(), 1);
    }

    #[test]
    fn an_empty_query_lists_the_newest_first_and_repeats_count_once() {
        let h = h();
        add(&h, "a", 1);
        add(&h, "b", 2);
        add(&h, "a", 3);
        let hits = h.search("", None, 10);
        assert_eq!(hits.iter().map(|x| (x.cmd.as_str(), x.count)).collect::<Vec<_>>(), vec![("a", 2), ("b", 1)]);
        assert_eq!(h.count().unwrap(), 3, "every use is a row, the search shows each command once");
    }

    #[test]
    fn among_equal_matches_the_recent_and_the_often_used_come_first() {
        let h = h();
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
        add(&h, "make old", now - 90 * 86_400);
        add(&h, "make new", now);
        let hits = h.search("make", None, 10);
        assert_eq!(hits[0].cmd, "make new");
    }

    #[test]
    fn a_host_restricts_the_search_and_delete_and_clear_forget() {
        let h = h();
        h.add("uptime", "/", "talos", Some(0), 1).unwrap();
        h.add("uptime", "/", "local", Some(0), 2).unwrap();
        h.add("ls", "/", "local", Some(0), 3).unwrap();
        assert_eq!(h.search("uptime", Some("talos"), 10).len(), 1);
        assert_eq!(h.search("uptime", None, 10).len(), 2);
        assert_eq!(h.delete("uptime").unwrap(), 2);
        assert!(h.search("uptime", None, 10).is_empty());
        assert_eq!(h.search("", None, 10).len(), 1);
        h.clear().unwrap();
        assert!(h.search("", None, 10).is_empty() && h.count().unwrap() == 0);
    }

    #[test]
    fn the_file_is_private_survives_a_restart_and_keeps_its_counts() {
        let dir = std::env::temp_dir().join(format!("hw-hist-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("history.db");
        {
            let h = History::open(&path).unwrap();
            add(&h, "ssh talos", 10);
            add(&h, "ssh talos", 20);
            h.add("ls", "/tmp", "local", Some(1), 5).unwrap();
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let h = History::open(&path).unwrap();
        let hits = h.search("", None, 10);
        assert_eq!((hits[0].cmd.as_str(), hits[0].count, hits[0].last), ("ssh talos", 2, 20));
        assert_eq!((hits[1].cwd.as_str(), hits[1].exit), ("/tmp", Some(1)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn past_the_bound_the_oldest_tenth_goes_from_the_file_and_the_search() {
        let h = h();
        for i in 0..100 {
            add(&h, &format!("cmd {i}"), i);
        }
        h.prune_over(100).unwrap();
        assert_eq!(h.count().unwrap(), 100, "not over the bound");
        h.prune_over(99).unwrap();
        assert_eq!(h.count().unwrap(), 91, "a tenth of the bound");
        assert!(h.search("cmd 5", None, 200).iter().all(|x| x.cmd != "cmd 5" && x.cmd != "cmd 8"), "the oldest are gone from the cache too");
        assert!(h.search("cmd 99", None, 5).iter().any(|x| x.cmd == "cmd 99"));
    }

    #[test]
    fn the_limit_is_bounded() {
        let h = h();
        for i in 0..300 {
            add(&h, &format!("cmd {i}"), i);
        }
        assert_eq!(h.search("", None, 5000).len(), MAX_HITS);
        assert_eq!(h.search("", None, 0).len(), 1);
    }
}
