//! The hidden-chat feature (chat invisible to members) was removed entirely: all members always see the chat.
//! This test fails if its setting, protocol field, state, UI or strings come back anywhere in
//! the source. The patterns are assembled at runtime so this file does not match itself.

use std::path::{Path, PathBuf};

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if p.is_dir() {
            if !matches!(
                name.as_str(),
                "target" | "node_modules" | "dist" | "gen" | ".git"
            ) {
                walk(&p, out);
            }
        } else if [".rs", ".ts", ".tsx", ".json"]
            .iter()
            .any(|x| name.ends_with(x))
        {
            out.push(p);
        }
    }
}

#[test]
fn no_hidden_chat_code_setting_or_string_remains() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let mut files = Vec::new();
    for d in ["crates", "apps/desktop/src", "apps/desktop/src-tauri/src"] {
        walk(&root.join(d), &mut files);
    }
    assert!(
        files.len() > 50,
        "found the source tree ({} files)",
        files.len()
    );
    let patterns = [
        format!("{}_{}", "hidden_from", "members"),
        format!("{}{}", "hiddenFrom", "Members"),
        format!("{} {}", "Hide", "chat"),
        format!("{} {}", "hide", "chat from"),
    ];
    let mut hits = Vec::new();
    for f in &files {
        let Ok(text) = std::fs::read_to_string(f) else {
            continue;
        };
        for p in &patterns {
            if text.contains(p.as_str()) {
                hits.push(format!("{}: {p}", f.display()));
            }
        }
    }
    assert!(
        hits.is_empty(),
        "hidden-chat code remains:\n{}",
        hits.join("\n")
    );
}
