//! Test-only helper: reads the developer's real secret VALUES from the git-ignored env files so
//! tests can assert they never leak. Never prints them.

#![cfg(test)]

use std::path::Path;

pub fn load_env_secret_values(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(text) = std::fs::read_to_string(root.join(".env.local")) {
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('#') {
                continue;
            }
            if let Some((_, v)) = line.split_once('=') {
                let v = v.trim().trim_matches('"').trim_matches('\'');
                if v.len() >= 6 {
                    out.push(v.to_owned());
                }
            }
        }
    }
    if let Ok(text) = std::fs::read_to_string(root.join(".env.ice.local")) {
        for key in ["username", "credential"] {
            let mut rest = text.as_str();
            while let Some(pos) = rest.find(key) {
                rest = &rest[pos + key.len()..];
                let after = rest.trim_start_matches(['"', '\'', ' ', '\t']);
                let Some(after) = after.strip_prefix(':') else {
                    continue;
                };
                let after = after.trim_start();
                let Some(q) = after.chars().next().filter(|c| *c == '"' || *c == '\'') else {
                    continue;
                };
                let body = &after[1..];
                if let Some(end) = body.find(q) {
                    let v = &body[..end];
                    if v.len() >= 6 && !out.iter().any(|x| x == v) {
                        out.push(v.to_owned());
                    }
                }
            }
        }
    }
    out
}
