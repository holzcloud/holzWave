//! The candidate as a stand-alone echo server on 127.0.0.1, for the Autobahn suite and the browser check.
//!
//!   candidate [port]
//!
//! Prints `listening 127.0.0.1:<port>` once it accepts connections and runs until it is stopped. Port 0 (the default)
//! lets the system pick one.

use std::net::SocketAddr;

#[tokio::main]
async fn main() {
    let port: u16 = std::env::args().nth(1).and_then(|p| p.parse().ok()).unwrap_or(0);
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let server = match web_deflate::candidate::serve_candidate(addr, Vec::new(), web_deflate::candidate::options()).await {
        Ok(server) => server,
        Err(e) => {
            eprintln!("candidate: {e}");
            std::process::exit(1);
        }
    };
    println!("listening {}", server.addr);
    std::future::pending::<()>().await;
}
