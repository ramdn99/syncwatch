//! ADR 0009 condition: the TLS provider is installed once, at startup, in one place, and no
//! second provider install can happen.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use syncwatch_ice_config::{ProviderInstall, install_crypto_provider, provider_is_ring};

#[test]
fn the_provider_is_installed_once_and_stays_ring() {
    let first = install_crypto_provider();
    assert_eq!(first, ProviderInstall::Installed);
    assert!(provider_is_ring());
    for _ in 0..3 {
        assert_eq!(install_crypto_provider(), ProviderInstall::AlreadyInstalled);
    }
    // A different provider can no longer become the default.
    let other = rustls::crypto::CryptoProvider {
        cipher_suites: vec![rustls::crypto::ring::cipher_suite::TLS13_AES_128_GCM_SHA256],
        ..rustls::crypto::ring::default_provider()
    };
    assert!(other.install_default().is_err(), "second install refused");
    assert!(provider_is_ring(), "still the ring default");
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        let n = e.file_name().to_string_lossy().into_owned();
        if p.is_dir() {
            if !matches!(
                n.as_str(),
                "target" | "node_modules" | "tests" | "examples" | "gen"
            ) {
                walk(&p, out);
            }
        } else if n.ends_with(".rs") {
            out.push(p);
        }
    }
}

#[test]
fn exactly_one_install_site_in_the_sources() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let mut files = Vec::new();
    walk(&root.join("crates"), &mut files);
    walk(
        &root
            .join("apps")
            .join("desktop")
            .join("src-tauri")
            .join("src"),
        &mut files,
    );
    assert!(files.len() > 30);
    let pat_install = format!("{}{}", ".install_", "default(");
    let pat_call = format!("{}{}", "install_crypto_", "provider()");
    let mut installs = Vec::new();
    let mut calls = Vec::new();
    for f in &files {
        let text = std::fs::read_to_string(f).unwrap_or_default();
        // Test modules (no app startup there) may install the provider for their own client.
        let text = text
            .split("#[cfg(test)]")
            .next()
            .unwrap_or_default()
            .to_owned();
        if text.contains(&pat_install) {
            installs.push(f.display().to_string());
        }
        if text.contains(&pat_call) && !f.ends_with("lib.rs") {
            calls.push(f.display().to_string());
        }
    }
    assert_eq!(
        installs.len(),
        1,
        "only ice-config installs a provider: {installs:?}"
    );
    assert!(installs[0].contains("ice-config"));
    assert_eq!(
        calls.len(),
        1,
        "one startup call site in the app: {calls:?}"
    );
}
