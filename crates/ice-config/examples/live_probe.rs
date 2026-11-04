//! Dev-only live check against the developer's real Metered account (Milestone 3 proof).
//!
//! Reads `.env.local` (METERED_APP, METERED_API_KEY) and `.env.ice.local` (ICE servers array)
//! from the repository root. Prints ONLY: per-URL transport label (no hostname), OK/FAIL,
//! latency, and whether the TURN allocation was released; plus traffic totals for the quota
//! report. Never prints credentials, app names, hostnames or addresses.
//!
//!   cargo run -p syncwatch-ice-config --example live_probe

#![allow(clippy::print_stdout)]

use std::path::Path;
use syncwatch_ice_config::*;

fn env_value(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|l| {
        let (k, v) = l.trim().split_once('=')?;
        (k.trim() == key).then(|| v.trim().trim_matches('"').trim_matches('\'').to_owned())
    })
}

fn print_report(title: &str, r: &TestReport) {
    println!("{title}");
    for (i, u) in r.results.iter().enumerate() {
        let status = if u.ok { "OK  " } else { "FAIL" };
        let lat = u
            .latency_ms
            .map(|l| format!("{l} ms"))
            .unwrap_or_else(|| "-".into());
        let extra = match (u.kind, u.ok) {
            (ProbeKind::Relay, true) => format!(" relay, released={}", u.released),
            (ProbeKind::Reflexive, true) => " stun".to_owned(),
            (_, false) => format!(" {:?}", u.error.unwrap_or(ProbeError::Protocol)),
        };
        println!("  #{i:<2} {:<16} {status} {lat:>7}{extra}", u.label);
    }
    println!(
        "  verified={}  allocations={} released={}  traffic: {} B sent, {} B received",
        r.verified, r.allocations, r.allocations_released, r.bytes_sent, r.bytes_received
    );
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    install_crypto_provider();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut failed = false;

    // Tab A: app name + API key -> fetch fresh credentials -> test.
    match std::fs::read_to_string(root.join(".env.local")) {
        Ok(text) => match (
            env_value(&text, "METERED_APP"),
            env_value(&text, "METERED_API_KEY"),
        ) {
            (Some(app), Some(key)) => {
                let creds = ApiCredentials { app, key };
                let t0 = std::time::Instant::now();
                match fetch_ice_servers(&creds).await {
                    Ok(servers) => {
                        println!(
                            "Tab A fetch: OK in {} ms ({} ICE servers, 1 API request)",
                            t0.elapsed().as_millis(),
                            servers.len()
                        );
                        let r = test_servers(&servers).await;
                        failed |= !r.verified || r.allocations != r.allocations_released;
                        print_report("Tab A connection test:", &r);
                    }
                    Err(e) => {
                        failed = true;
                        println!("Tab A fetch: FAIL ({e:?})");
                    }
                }
            }
            _ => println!("Tab A: skipped (.env.local lacks METERED_APP / METERED_API_KEY)"),
        },
        Err(_) => println!("Tab A: skipped (.env.local not found)"),
    }

    // Tab B: pasted ICE servers array -> lenient parse -> test.
    match std::fs::read_to_string(root.join(".env.ice.local")) {
        Ok(text) => match parse_ice_servers(&text) {
            Ok(servers) => {
                println!("Tab B parse: OK ({} ICE servers)", servers.len());
                let r = test_servers(&servers).await;
                failed |= !r.verified || r.allocations != r.allocations_released;
                print_report("Tab B connection test:", &r);
            }
            Err(e) => {
                failed = true;
                println!("Tab B parse: FAIL ({e})");
            }
        },
        Err(_) => println!("Tab B: skipped (.env.ice.local not found)"),
    }
    if failed {
        std::process::exit(1);
    }
}
