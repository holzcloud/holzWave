//! Phase 1 spike: measurements that decide the engine and renderer.
//!
//!   holzwave-spike screen <cmd> [args…]   run a command in a PTY, print the final screen
//!   holzwave-spike throughput [MiB]       feed synthetic output into the engine
//!   holzwave-spike latency [N]            PTY round trip through `cat`

use std::io::Read;
use std::process::Command;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use holzwave_core::pty::Pty;
use holzwave_core::{GhosttyEngine, TerminalEngine};

const COLS: u16 = 120;
const ROWS: u16 = 40;

fn rss_mib() -> f64 {
    let out = Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok();
    out.and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse::<f64>().ok())
        .map_or(f64::NAN, |kib| kib / 1024.0)
}

fn spawn_reader(mut r: Box<dyn Read + Send>) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = vec![0u8; 64 * 1024];
        while let Ok(n) = r.read(&mut buf) {
            if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });
    rx
}

fn screen(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let (prog, rest) = args.split_first().ok_or("usage: screen <cmd> [args…]")?;
    let rest: Vec<&str> = rest.iter().map(String::as_str).collect();
    let mut pty = Pty::spawn(prog, &rest, COLS, ROWS)?;
    let rx = spawn_reader(pty.take_reader().ok_or("reader")?);
    let mut engine = GhosttyEngine::new(COLS, ROWS, 10_000)?;
    // Drain until the child closes the PTY or goes quiet.
    while let Ok(chunk) = rx.recv_timeout(Duration::from_secs(2)) {
        engine.feed(&chunk);
    }
    let _ = pty.wait();
    print!("{}", engine.screen()?.to_text());
    Ok(())
}

/// A line that looks like real output: a path, colour changes and some Unicode.
fn synthetic_chunk() -> Vec<u8> {
    let mut v = Vec::new();
    for i in 0..256 {
        v.extend_from_slice(
            format!(
                "\x1b[32m./src/module_{i:03}/handler.rs\x1b[0m:\x1b[33m{}\x1b[0m: \x1b[1merror\x1b[0m[E{:04}]: größe überschritten → 日本語\r\n",
                i * 7,
                i
            )
            .as_bytes(),
        );
    }
    v
}

fn throughput(mib: usize) -> Result<(), Box<dyn std::error::Error>> {
    let chunk = synthetic_chunk();
    let total = mib * 1024 * 1024;
    let rss0 = rss_mib();

    let mut engine = GhosttyEngine::new(COLS, ROWS, 10_000)?;
    let start = Instant::now();
    let mut fed = 0;
    while fed < total {
        engine.feed(&chunk);
        fed += chunk.len();
    }
    let parse = start.elapsed();

    // Same again, but with a screen copy after every chunk (≈ one frame per 16 KiB).
    let mut engine = GhosttyEngine::new(COLS, ROWS, 10_000)?;
    let start = Instant::now();
    let mut fed = 0;
    let mut frames = 0;
    while fed < total {
        engine.feed(&chunk);
        fed += chunk.len();
        let _ = engine.screen()?;
        frames += 1;
    }
    let with_frames = start.elapsed();

    let mib_s = |d: Duration| fed as f64 / 1048576.0 / d.as_secs_f64();
    println!("input            {} MiB, {} B chunks", fed / 1048576, chunk.len());
    println!("parse only       {:>8.1} MiB/s  ({:.2?})", mib_s(parse), parse);
    println!(
        "parse + screen   {:>8.1} MiB/s  ({:.2?}, {frames} screen copies, {:.0} µs each)",
        mib_s(with_frames),
        with_frames,
        with_frames.as_micros() as f64 / frames as f64
    );
    println!("rss              {:.0} MiB (start {:.0} MiB)", rss_mib(), rss0);
    Ok(())
}

fn latency(n: usize) -> Result<(), Box<dyn std::error::Error>> {
    let mut pty = Pty::spawn("/bin/cat", &[], COLS, ROWS)?;
    let rx = spawn_reader(pty.take_reader().ok_or("reader")?);
    let mut engine = GhosttyEngine::new(COLS, ROWS, 1000)?;
    let mut samples = Vec::with_capacity(n);
    for i in 0..n {
        let ch = b'a' + (i % 26) as u8;
        let t = Instant::now();
        pty.write(&[ch])?;
        // `cat` echoes via the tty line discipline and again as output; wait for the first byte back.
        let chunk = rx.recv_timeout(Duration::from_secs(1))?;
        engine.feed(&chunk);
        let _ = engine.screen()?;
        samples.push(t.elapsed());
        // Drain the rest of this round before the next one.
        while let Ok(c) = rx.recv_timeout(Duration::from_millis(5)) {
            engine.feed(&c);
        }
    }
    samples.sort();
    let p = |q: f64| samples[((samples.len() - 1) as f64 * q) as usize];
    println!("round trips      {n}");
    println!("write → screen   p50 {:.0?}  p95 {:.0?}  p99 {:.0?}  max {:.0?}", p(0.5), p(0.95), p(0.99), samples[samples.len() - 1]);
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("screen") => screen(&args[1..]),
        Some("throughput") => throughput(args.get(1).and_then(|s| s.parse().ok()).unwrap_or(256)),
        Some("latency") => latency(args.get(1).and_then(|s| s.parse().ok()).unwrap_or(500)),
        _ => {
            eprintln!("usage: holzwave-spike screen <cmd> [args…] | throughput [MiB] | latency [N]");
            std::process::exit(2);
        }
    }
}
