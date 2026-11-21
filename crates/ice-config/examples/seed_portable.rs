//! Dev tool (two-release-instance labs): saves the developer's Metered app name + API key from
//! `.env.local` into a PORTABLE SyncWatch data folder (`<data>\config\credentials.dpapi`,
//! DPAPI-encrypted for the current Windows user), exactly as the app's setup screen would.
//! Prints nothing but "seeded" or a reason. The values never leave this process.
//!
//!   cargo run -p syncwatch-ice-config --example seed_portable -- <portable folder>\data

#![allow(clippy::print_stdout)] // a dev tool: its only output is "seeded"

use std::path::{Path, PathBuf};
use syncwatch_ice_config::store::{DpapiFileStore, StoredCredentials, save_credentials};

fn env_value(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|l| {
        let (k, v) = l.trim().split_once('=')?;
        (k.trim() == key).then(|| v.trim().trim_matches('"').trim_matches('\'').to_owned())
    })
}

fn main() {
    let Some(data) = std::env::args().nth(1).map(PathBuf::from) else {
        eprintln!("usage: seed_portable <portable data folder>");
        std::process::exit(2);
    };
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let Ok(text) = std::fs::read_to_string(root.join(".env.local")) else {
        eprintln!(".env.local not found");
        std::process::exit(1);
    };
    let (Some(app), Some(key)) = (
        env_value(&text, "METERED_APP"),
        env_value(&text, "METERED_API_KEY"),
    ) else {
        eprintln!(".env.local lacks METERED_APP / METERED_API_KEY");
        std::process::exit(1);
    };
    let config = data.join("config");
    if std::fs::create_dir_all(&config).is_err() {
        eprintln!("cannot create the config folder");
        std::process::exit(1);
    }
    let store = DpapiFileStore {
        path: config.join("credentials.dpapi"),
    };
    match save_credentials(&store, &StoredCredentials::ApiKey { app, key }) {
        Ok(()) => println!("seeded"),
        Err(_) => {
            eprintln!("could not save");
            std::process::exit(1);
        }
    }
}
