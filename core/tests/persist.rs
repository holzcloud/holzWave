//! Tests for the persistence primitives: atomic writes, bounded reads, backups and the debounced saver.

mod common;

use std::io::ErrorKind;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, ThreadId};
use std::time::Duration;

use common::{scratch_dir, wait_until};
use holzwave_core::persist::{Debounced, backup_bad, bad_path, read_limited, write_atomic};

/// Names of everything in `dir`, sorted.
fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> =
        std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    v.sort();
    v
}

// ---- write_atomic ----------------------------------------------------------------------------------

#[test]
fn write_atomic_creates_parents_writes_and_leaves_no_temporary_file() {
    let dir = scratch_dir("atomic-create");
    let path = dir.join("a").join("b").join("layout.json");
    write_atomic(&path, b"{\"v\":1}").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"{\"v\":1}");
    assert_eq!(names(path.parent().unwrap()), vec!["layout.json"]);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn write_atomic_replaces_an_existing_file_completely() {
    let dir = scratch_dir("atomic-replace");
    let path = dir.join("settings.json");
    write_atomic(&path, &vec![b'x'; 10_000]).unwrap();
    write_atomic(&path, b"short").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"short", "no tail of the longer old content");
    assert_eq!(names(&dir), vec!["settings.json"]);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn write_atomic_reports_failure_and_cleans_up_its_temporary_file() {
    let dir = scratch_dir("atomic-fail");
    // The target is a directory: the final rename cannot succeed.
    let target = dir.join("layout.json");
    std::fs::create_dir(&target).unwrap();
    assert!(write_atomic(&target, b"data").is_err());
    assert_eq!(names(&dir), vec!["layout.json"], "no .tmp left behind");

    // A parent that is a file: the directory cannot be created.
    let file = dir.join("plain");
    std::fs::write(&file, b"x").unwrap();
    assert!(write_atomic(&file.join("inner.json"), b"data").is_err());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn write_atomic_replaces_a_stale_temporary_file() {
    let dir = scratch_dir("atomic-stale");
    let path = dir.join("layout.json");
    std::fs::write(dir.join("layout.json.tmp"), b"half a write from a crash").unwrap();
    write_atomic(&path, b"fresh").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"fresh");
    assert_eq!(names(&dir), vec!["layout.json"]);
    std::fs::remove_dir_all(dir).unwrap();
}

// ---- read_limited ----------------------------------------------------------------------------------

#[test]
fn read_limited_distinguishes_missing_within_limit_and_too_large() {
    let dir = scratch_dir("read-limited");
    let path = dir.join("layout.json");
    assert_eq!(read_limited(&path, 16).unwrap(), None, "missing file");

    std::fs::write(&path, b"0123456789abcdef").unwrap();
    assert_eq!(read_limited(&path, 16).unwrap().as_deref(), Some(&b"0123456789abcdef"[..]), "exactly at the limit");

    std::fs::write(&path, b"0123456789abcdefX").unwrap();
    let err = read_limited(&path, 16).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidData);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn read_limited_does_not_read_a_file_that_is_over_the_limit() {
    let dir = scratch_dir("read-huge");
    let path = dir.join("huge.json");
    // A sparse file of 1 TiB: reading it into memory would fail or take forever; the length check must not.
    let f = std::fs::File::create(&path).unwrap();
    f.set_len(1 << 40).unwrap();
    drop(f);
    let err = read_limited(&path, 1 << 20).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidData);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn read_limited_refuses_what_is_not_a_regular_file() {
    let dir = scratch_dir("read-dir");
    assert!(read_limited(&dir, 1024).is_err());
    std::fs::remove_dir_all(dir).unwrap();
}

// ---- backup_bad ------------------------------------------------------------------------------------

#[test]
fn backup_bad_copies_next_to_the_file_and_overwrites_an_older_backup() {
    let dir = scratch_dir("backup");
    let path = dir.join("layout.json");
    std::fs::write(&path, b"first damage").unwrap();
    let bad = backup_bad(&path).unwrap();
    assert_eq!(bad, dir.join("layout.json.bad"));
    assert_eq!(bad, bad_path(&path));
    assert_eq!(std::fs::read(&bad).unwrap(), b"first damage");

    std::fs::write(&path, b"second").unwrap();
    backup_bad(&path).unwrap();
    assert_eq!(std::fs::read(&bad).unwrap(), b"second", "the older backup is replaced");
    assert_eq!(std::fs::read(&path).unwrap(), b"second", "the original stays");
    assert!(backup_bad(&dir.join("missing.json")).is_err());
    std::fs::remove_dir_all(dir).unwrap();
}

// ---- Debounced -------------------------------------------------------------------------------------

fn counter() -> (Arc<AtomicUsize>, impl FnMut() + Send + 'static) {
    let n = Arc::new(AtomicUsize::new(0));
    let n2 = Arc::clone(&n);
    (n, move || {
        n2.fetch_add(1, Ordering::SeqCst);
    })
}

#[test]
fn a_thousand_touches_run_the_job_at_most_twice() {
    let (n, job) = counter();
    let d = Debounced::new(Duration::from_millis(40), job);
    for _ in 0..1000 {
        d.touch();
    }
    assert!(wait_until(Duration::from_secs(3), || n.load(Ordering::SeqCst) >= 1), "the job ran");
    thread::sleep(Duration::from_millis(200));
    let runs = n.load(Ordering::SeqCst);
    assert!((1..=2).contains(&runs), "ran {runs} times");
}

#[test]
fn nothing_runs_without_a_touch() {
    let (n, job) = counter();
    let d = Debounced::new(Duration::from_millis(10), job);
    thread::sleep(Duration::from_millis(100));
    d.flush();
    assert_eq!(n.load(Ordering::SeqCst), 0);
}

#[test]
fn flush_runs_a_pending_job_once_on_the_calling_thread() {
    let ran: Arc<Mutex<Vec<ThreadId>>> = Arc::default();
    let ran2 = Arc::clone(&ran);
    let d = Debounced::new(Duration::from_secs(30), move || ran2.lock().unwrap().push(thread::current().id()));
    d.touch();
    d.touch();
    d.flush();
    assert_eq!(*ran.lock().unwrap(), vec![thread::current().id()], "once, here");
    d.flush();
    assert_eq!(ran.lock().unwrap().len(), 1, "a second flush finds nothing pending");
}

#[test]
fn a_flushed_job_is_not_run_again_by_the_timer() {
    let (n, job) = counter();
    let d = Debounced::new(Duration::from_millis(50), job);
    d.touch();
    d.flush();
    thread::sleep(Duration::from_millis(250));
    assert_eq!(n.load(Ordering::SeqCst), 1);
}

#[test]
fn dropping_the_saver_flushes_a_pending_job() {
    let (n, job) = counter();
    let d = Debounced::new(Duration::from_secs(30), job);
    d.touch();
    drop(d);
    assert_eq!(n.load(Ordering::SeqCst), 1);

    let (n, job) = counter();
    let d = Debounced::new(Duration::from_secs(30), job);
    drop(d);
    assert_eq!(n.load(Ordering::SeqCst), 0, "nothing pending, nothing run");
}

#[test]
fn continuous_touching_still_persists_every_delay() {
    let (n, job) = counter();
    let d = Debounced::new(Duration::from_millis(40), job);
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_millis(400) {
        d.touch();
        thread::sleep(Duration::from_millis(1));
    }
    let runs = n.load(Ordering::SeqCst);
    assert!(runs >= 3, "a drag that never pauses must not starve the saver, got {runs} runs in 400 ms");
}

#[test]
fn the_job_never_runs_concurrently_with_a_flush() {
    let running = Arc::new(AtomicUsize::new(0));
    let overlap = Arc::new(AtomicUsize::new(0));
    let runs = Arc::new(AtomicUsize::new(0));
    let (r, o, n) = (Arc::clone(&running), Arc::clone(&overlap), Arc::clone(&runs));
    let d = Arc::new(Debounced::new(Duration::from_millis(5), move || {
        if r.fetch_add(1, Ordering::SeqCst) != 0 {
            o.fetch_add(1, Ordering::SeqCst);
        }
        thread::sleep(Duration::from_millis(15));
        r.fetch_sub(1, Ordering::SeqCst);
        n.fetch_add(1, Ordering::SeqCst);
    }));
    let flusher = {
        let d = Arc::clone(&d);
        thread::spawn(move || {
            for _ in 0..40 {
                d.touch();
                d.flush();
                thread::sleep(Duration::from_millis(2));
            }
        })
    };
    for _ in 0..40 {
        d.touch();
        thread::sleep(Duration::from_millis(3));
    }
    flusher.join().unwrap();
    d.flush();
    assert_eq!(overlap.load(Ordering::SeqCst), 0, "two runs overlapped");
    assert!(runs.load(Ordering::SeqCst) >= 2);
}
