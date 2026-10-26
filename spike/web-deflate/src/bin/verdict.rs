//! Reads `results.json` and writes `VERDICT` next to the manifest: `pass` or `fail`, then one line per criterion.

fn main() {
    let results = web_deflate::results::load();
    let verdict = web_deflate::verdict::verdict(&results);
    let text = verdict.text();
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("VERDICT");
    if let Err(e) = std::fs::write(&path, &text) {
        eprintln!("verdict: {}: {e}", path.display());
        std::process::exit(1);
    }
    print!("{text}");
}
