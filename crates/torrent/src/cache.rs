//! Torrent cache (spec section 10): one folder per infohash under the configured location, a
//! registry of FINISHED files (a restart or rejoin plays them directly and never re-adds the
//! torrent to a swarm, ADR 0010), a size cap, and cleanup when leaving.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use syncwatch_protocol::Infohash;

const REGISTRY: &str = "finished.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinishedEntry {
    pub infohash: String,
    pub file_index: u32,
    pub size: u64,
    /// Path relative to the torrent's folder.
    pub rel_path: String,
    /// Monotonic order of completion (for eviction).
    pub seq: u64,
}

#[derive(Debug)]
pub struct Cache {
    dir: PathBuf,
    cap_bytes: u64,
    entries: Vec<FinishedEntry>,
}

pub fn hex(ih: &Infohash) -> String {
    ih.to_hex()
}

fn dir_size(p: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(p) else {
        return 0;
    };
    rd.flatten()
        .map(|e| match e.metadata() {
            Ok(m) if m.is_dir() => dir_size(&e.path()),
            Ok(m) => m.len(),
            Err(_) => 0,
        })
        .sum()
}

impl Cache {
    pub fn open(dir: PathBuf, cap_bytes: u64) -> Self {
        let _ = std::fs::create_dir_all(&dir);
        let entries = std::fs::read(dir.join(REGISTRY))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Self {
            dir,
            cap_bytes,
            entries,
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn set_cap(&mut self, cap_bytes: u64) {
        self.cap_bytes = cap_bytes;
    }

    pub fn torrent_dir(&self, ih: &Infohash) -> PathBuf {
        self.dir.join(hex(ih))
    }

    fn save(&self) {
        if let Ok(b) = serde_json::to_vec_pretty(&self.entries) {
            let tmp = self.dir.join("finished.json.tmp");
            if std::fs::write(&tmp, b).is_ok() {
                let _ = std::fs::rename(&tmp, self.dir.join(REGISTRY));
            }
        }
    }

    /// A finished file for this torrent/file that is still on disk with the right size.
    pub fn finished(&self, ih: &Infohash, file_index: u32, size: u64) -> Option<PathBuf> {
        let h = hex(ih);
        let e = self
            .entries
            .iter()
            .find(|e| e.infohash == h && e.file_index == file_index && e.size == size)?;
        let p = self.torrent_dir(ih).join(&e.rel_path);
        (std::fs::metadata(&p).ok()?.len() == size).then_some(p)
    }

    pub fn record_finished(&mut self, ih: &Infohash, file_index: u32, size: u64, rel_path: &str) {
        let h = hex(ih);
        self.entries
            .retain(|e| !(e.infohash == h && e.file_index == file_index));
        let seq = self.entries.iter().map(|e| e.seq + 1).max().unwrap_or(1);
        self.entries.push(FinishedEntry {
            infohash: h,
            file_index,
            size,
            rel_path: rel_path.to_owned(),
            seq,
        });
        self.save();
    }

    /// Deletes a torrent's folder and its registry entries.
    pub fn remove(&mut self, ih: &Infohash) {
        let h = hex(ih);
        let _ = std::fs::remove_dir_all(self.torrent_dir(ih));
        let before = self.entries.len();
        self.entries.retain(|e| e.infohash != h);
        if self.entries.len() != before {
            self.save();
        }
    }

    /// Keeps only `keep` (relative path) inside the torrent folder: boundary pieces write bytes
    /// of neighbouring files (ADR 0009), which are deleted once the selected file is complete.
    pub fn prune_except(&self, ih: &Infohash, keep: &str) {
        let root = self.torrent_dir(ih);
        let keep = root.join(keep);
        fn walk(p: &Path, keep: &Path) {
            let Ok(rd) = std::fs::read_dir(p) else { return };
            for e in rd.flatten() {
                let path = e.path();
                if path.is_dir() {
                    walk(&path, keep);
                    let _ = std::fs::remove_dir(&path); // only if empty
                } else if path != keep {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
        walk(&root, &keep);
    }

    /// Bytes in torrent folders (the registry itself does not count).
    pub fn total_bytes(&self) -> u64 {
        let Ok(rd) = std::fs::read_dir(&self.dir) else {
            return 0;
        };
        rd.flatten()
            .filter(|e| e.path().is_dir())
            .map(|e| dir_size(&e.path()))
            .sum()
    }

    /// Evicts the oldest finished torrents (never those in `protect`) until under the cap.
    /// Returns the evicted infohashes (hex).
    pub fn enforce_cap(&mut self, protect: &[Infohash]) -> Vec<String> {
        let protect: Vec<String> = protect.iter().map(hex).collect();
        let mut evicted = Vec::new();
        let mut order: Vec<FinishedEntry> = self.entries.clone();
        order.sort_by_key(|e| e.seq);
        let mut total = self.total_bytes();
        for e in order {
            if total <= self.cap_bytes {
                break;
            }
            if protect.contains(&e.infohash) || evicted.contains(&e.infohash) {
                continue;
            }
            let d = self.dir.join(&e.infohash);
            total = total.saturating_sub(dir_size(&d));
            let _ = std::fs::remove_dir_all(&d);
            evicted.push(e.infohash.clone());
        }
        if !evicted.is_empty() {
            self.entries.retain(|e| !evicted.contains(&e.infohash));
            self.save();
        }
        evicted
    }

    /// Whether `bytes` more fit under the cap (after evicting what may be evicted).
    pub fn fits(&mut self, bytes: u64, protect: &[Infohash]) -> bool {
        self.enforce_cap(protect);
        self.total_bytes().saturating_add(bytes) <= self.cap_bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("sw-cache-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn finished_registry_survives_reopen_and_checks_the_size() {
        let d = tmp("reg");
        let ih = Infohash([1; 20]);
        let mut c = Cache::open(d.clone(), u64::MAX);
        let f = c.torrent_dir(&ih).join("Movie").join("a.mkv");
        std::fs::create_dir_all(f.parent().unwrap_or(&d)).unwrap_or_else(|e| panic!("{e}"));
        std::fs::write(&f, vec![0u8; 100]).unwrap_or_else(|e| panic!("{e}"));
        c.record_finished(&ih, 0, 100, "Movie/a.mkv");
        let c2 = Cache::open(d.clone(), u64::MAX);
        assert_eq!(c2.finished(&ih, 0, 100), Some(f.clone()));
        assert_eq!(c2.finished(&ih, 0, 99), None, "size must match");
        assert_eq!(c2.finished(&ih, 1, 100), None);
        std::fs::write(&f, vec![0u8; 50]).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            c2.finished(&ih, 0, 100),
            None,
            "truncated file is not finished"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn prune_keeps_only_the_selected_file() {
        let d = tmp("prune");
        let ih = Infohash([2; 20]);
        let c = Cache::open(d.clone(), u64::MAX);
        let root = c.torrent_dir(&ih).join("T");
        std::fs::create_dir_all(root.join("extras")).unwrap_or_else(|e| panic!("{e}"));
        for f in ["a.mkv", "b.nfo", "extras/c.mkv"] {
            std::fs::write(root.join(f), b"x").unwrap_or_else(|e| panic!("{e}"));
        }
        c.prune_except(&ih, "T/a.mkv");
        assert!(root.join("a.mkv").exists());
        assert!(!root.join("b.nfo").exists());
        assert!(!root.join("extras").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn cap_evicts_oldest_finished_but_not_protected() {
        let d = tmp("cap");
        let mut c = Cache::open(d.clone(), 250);
        for (i, ih) in [Infohash([3; 20]), Infohash([4; 20]), Infohash([5; 20])]
            .iter()
            .enumerate()
        {
            let p = c.torrent_dir(ih);
            std::fs::create_dir_all(&p).unwrap_or_else(|e| panic!("{e}"));
            std::fs::write(p.join("f.mkv"), vec![0u8; 100]).unwrap_or_else(|e| panic!("{e}"));
            c.record_finished(ih, i as u32, 100, "f.mkv");
        }
        let ev = c.enforce_cap(&[Infohash([3; 20])]);
        assert_eq!(
            ev,
            vec![hex(&Infohash([4; 20]))],
            "oldest unprotected first"
        );
        assert_eq!(c.total_bytes(), 200);
        assert!(!c.fits(1_000, &[]));
        let _ = std::fs::remove_dir_all(&d);
    }
}
