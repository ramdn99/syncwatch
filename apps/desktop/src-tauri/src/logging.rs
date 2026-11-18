//! Logging with a scrubbing writer (spec section 2, pitfall 1).
//!
//! - Third-party crates are capped at `warn`, whatever `SYNCWATCH_LOG` says.
//! - Every formatted log line passes through [`Scrubber`], which replaces any registered
//!   secret value (credentials, API key, Metered app name) with `<redacted>`.

use std::io::{self, Write};
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::{EnvFilter, prelude::*};

const REDACTED: &str = "<redacted>";
/// Values shorter than this are not registered (they would redact ordinary words).
#[cfg_attr(not(test), allow(dead_code))]
const MIN_SECRET_LEN: usize = 4;
const APP_CRATES: &[&str] = &["syncwatch", "syncwatch_desktop", "syncwatch_player"];

static SECRETS: RwLock<Vec<String>> = RwLock::new(Vec::new());

/// Registers a secret so it never appears in logs. Call it as soon as a secret is loaded.
/// (Credentials are loaded from Milestone 3 on; until then only tests call this.)
#[cfg_attr(not(test), allow(dead_code))]
pub fn register_secret(value: &str) {
    let v = value.trim();
    if v.len() < MIN_SECRET_LEN {
        return;
    }
    let mut g = SECRETS.write().unwrap_or_else(|p| p.into_inner());
    if !g.iter().any(|s| s == v) {
        g.push(v.to_owned());
        // Longest first, so a secret containing another is redacted whole.
        g.sort_by_key(|s| std::cmp::Reverse(s.len()));
    }
}

/// Shortens every absolute Windows path in `text` to its last component (`...\name.mkv`), so
/// logs carry no full paths (spec section 11): a path can contain the Windows user name and
/// the user's folder layout. Covers `C:\..`, `C:/..`, UNC `\\server\..` and the escaped Debug
/// form (`"C:\\Users\\.."`). A path ends at a quote, a line break, a tab, `|`, `<`, `>`, or where
/// the next path starts; unquoted paths may contain spaces, so a path is cut at its LAST
/// separator before that end (with several unquoted paths on one line more text goes, never less).
pub fn shorten_paths(text: &str) -> String {
    let b = text.as_bytes();
    let sep = |c: u8| c == b'\\' || c == b'/';
    let starts_at = |i: usize| -> bool {
        let prev_ok = i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'\\');
        let drive =
            i + 2 < b.len() && b[i].is_ascii_alphabetic() && b[i + 1] == b':' && sep(b[i + 2]);
        let unc = i + 2 < b.len() && b[i] == b'\\' && b[i + 1] == b'\\' && b[i + 2] != b' ';
        prev_ok && (drive || unc)
    };
    let hard_end = |c: u8| {
        matches!(
            c,
            b'"' | b'\'' | b'\n' | b'\r' | b'\t' | b'|' | b'<' | b'>' | b'`'
        )
    };
    let mut out = String::with_capacity(text.len());
    let (mut i, mut copied) = (0, 0);
    while i < b.len() {
        if !starts_at(i) {
            i += 1;
            continue;
        }
        let mut end = i + 2;
        while end < b.len() && !hard_end(b[end]) && !(b[end] == b' ' && starts_at(end + 1)) {
            end += 1;
        }
        // The last separator inside the path (there is at least the one after the drive).
        let last = (i..end).rev().find(|&k| sep(b[k])).unwrap_or(i + 2);
        out.push_str(&text[copied..i]);
        out.push_str("...\\");
        copied = last + 1;
        i = end;
    }
    out.push_str(&text[copied..]);
    out
}

/// Replaces all registered secrets in `text` and shortens absolute paths.
pub fn scrub(text: &str) -> String {
    let g = SECRETS.read().unwrap_or_else(|p| p.into_inner());
    let mut out = shorten_paths(text);
    for s in g.iter() {
        if out.contains(s.as_str()) {
            out = out.replace(s.as_str(), REDACTED);
        }
        let lower = s.to_lowercase();
        if lower != *s && out.contains(lower.as_str()) {
            out = out.replace(lower.as_str(), REDACTED);
        }
    }
    out
}

/// A `Write` adapter that scrubs each buffer before forwarding it.
pub struct Scrubber<W: Write> {
    inner: W,
}

impl<W: Write> Write for Scrubber<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let text = String::from_utf8_lossy(buf);
        self.inner.write_all(scrub(&text).as_bytes())?;
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// `MakeWriter` over a shared sink, wrapped in [`Scrubber`].
#[derive(Clone)]
pub struct ScrubbingMakeWriter<W: Write + Send + 'static> {
    sink: Arc<Mutex<W>>,
}

impl<W: Write + Send + 'static> ScrubbingMakeWriter<W> {
    pub fn new(sink: W) -> Self {
        Self {
            sink: Arc::new(Mutex::new(sink)),
        }
    }
}

pub struct LockedSink<W: Write + Send + 'static> {
    sink: Arc<Mutex<W>>,
}

impl<W: Write + Send + 'static> Write for LockedSink<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut g = self.sink.lock().unwrap_or_else(|p| p.into_inner());
        g.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.sink.lock().unwrap_or_else(|p| p.into_inner()).flush()
    }
}

impl<'a, W: Write + Send + 'static> MakeWriter<'a> for ScrubbingMakeWriter<W> {
    type Writer = Scrubber<LockedSink<W>>;
    fn make_writer(&'a self) -> Self::Writer {
        Scrubber {
            inner: LockedSink {
                sink: Arc::clone(&self.sink),
            },
        }
    }
}

/// Builds the filter: third-party crates at `warn`, app crates at `app_level`.
pub fn build_filter(app_level: &str) -> EnvFilter {
    let level = match app_level.to_ascii_lowercase().as_str() {
        l @ ("trace" | "debug" | "info" | "warn" | "error") => l.to_owned(),
        _ => "info".to_owned(),
    };
    let mut directives = String::from("warn");
    for c in APP_CRATES {
        directives.push_str(&format!(",{c}={level}"));
    }
    EnvFilter::new(directives)
}

/// Installs the global subscriber, writing to `<logs>/syncwatch.log` (rotated once at 10 MB)
/// and, in debug builds, to stderr. Both outputs are scrubbed.
pub fn init(logs_dir: &Path) {
    let level = std::env::var("SYNCWATCH_LOG").unwrap_or_else(|_| "info".to_owned());
    let path = logs_dir.join("syncwatch.log");
    if std::fs::metadata(&path)
        .map(|m| m.len() > 10 * 1024 * 1024)
        .unwrap_or(false)
    {
        let _ = std::fs::rename(&path, logs_dir.join("syncwatch.log.1"));
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path);

    let registry = tracing_subscriber::registry().with(build_filter(&level));
    let file_layer = file.ok().map(|f| {
        tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(ScrubbingMakeWriter::new(f))
    });
    let stderr_layer = cfg!(debug_assertions).then(|| {
        tracing_subscriber::fmt::layer().with_writer(ScrubbingMakeWriter::new(io::stderr()))
    });
    let _ = registry.with(file_layer).with(stderr_layer).try_init();
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use tracing::subscriber::with_default;

    /// Captures everything logged at the most verbose level through the scrubbing writer.
    pub(crate) fn capture(f: impl FnOnce()) -> String {
        let buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        struct Shared(Arc<Mutex<Vec<u8>>>);
        impl Write for Shared {
            fn write(&mut self, b: &[u8]) -> io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let sub = tracing_subscriber::registry()
            .with(EnvFilter::new("trace"))
            .with(
                tracing_subscriber::fmt::layer()
                    .with_ansi(false)
                    .with_writer(ScrubbingMakeWriter::new(Shared(Arc::clone(&buf)))),
            );
        with_default(sub, f);
        let bytes = buf.lock().unwrap().clone();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    #[test]
    fn synthetic_secrets_are_scrubbed_at_trace() {
        register_secret("TestAppName42");
        register_secret("FAKE-SECRET-VALUE-for-log-test-0001");
        let out = capture(|| {
            tracing::trace!(host = "testappname42.metered.live", "connecting");
            tracing::debug!("key=FAKE-SECRET-VALUE-for-log-test-0001 in url");
            tracing::warn!(target: "some_dependency", "TestAppName42 leaked by a dependency");
        });
        assert!(!out.contains("TestAppName42"));
        assert!(!out.to_lowercase().contains("testappname42"));
        assert!(!out.contains("FAKE-SECRET-VALUE-for-log-test-0001"));
        assert!(out.contains(REDACTED));
    }

    #[test]
    fn logs_carry_no_full_paths() {
        let cases = [
            (
                r"Cannot open file 'C:\Users\John Doe\Videos\My Movie.mkv'",
                r"Cannot open file '...\My Movie.mkv'",
            ),
            (
                r"native libraries loaded libmpv=C:\Program Files\SyncWatch\libmpv-2.dll vulkan=System",
                r"native libraries loaded libmpv=...\libmpv-2.dll vulkan=System",
            ),
            (
                r#"webview2=Some("C:\\Users\\jdoe\\AppData\\Local\\SyncWatch\\webview2") ok"#,
                r#"webview2=Some("...\webview2") ok"#,
            ),
            (r"open D:/media/a.mkv", r"open ...\a.mkv"),
            (r"share \\nas\films\b.mkv done", r"share ...\b.mkv done"),
            (r"two C:\a\x.srt D:\b\y.srt", r"two ...\x.srt ...\y.srt"),
            // Not paths: URLs, times, ratios, plain words.
            (
                "http://127.0.0.1:8080/x at 12:30 ratio 4:3",
                "http://127.0.0.1:8080/x at 12:30 ratio 4:3",
            ),
            ("nothing here", "nothing here"),
        ];
        for (input, want) in cases {
            assert_eq!(shorten_paths(input), want, "{input}");
        }
        let out = capture(|| {
            tracing::warn!(target: "mpv", r"cannot load C:\Users\jdoe\Videos\x.mkv");
            tracing::info!(file = ?std::path::Path::new(r"C:\Users\jdoe\Videos\x.mkv"), "opened");
        });
        assert!(!out.contains("jdoe"), "{out}");
        assert!(out.contains(r"...\x.mkv"));
    }

    #[test]
    fn short_values_are_not_registered() {
        register_secret("ab");
        assert_eq!(scrub("ab cd"), "ab cd");
    }

    #[test]
    fn third_party_capped_at_warn() {
        let f = build_filter("trace").to_string();
        assert!(
            f.split(',').any(|d| d == "warn"),
            "default directive must be warn"
        );
        assert!(f.contains("syncwatch_desktop=trace"));
        let bad = build_filter("verbose; drop table").to_string();
        assert!(bad.contains("syncwatch_desktop=info"));
    }

    /// Greps logs produced at the most verbose level for the developer's REAL secret values
    /// from the git-ignored env files. Values are never printed, only their index.
    #[test]
    fn real_env_secrets_never_reach_logs() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let values = crate::secrets_for_tests::load_env_secret_values(&root);
        if values.is_empty() {
            eprintln!("no env secret files present; real-value log test skipped");
            return;
        }
        for v in &values {
            register_secret(v);
        }
        let out = capture(|| {
            for v in &values {
                tracing::trace!("value {v} host {v}.metered.live");
                tracing::error!(target: "reqwest", url = %format!("https://{v}.metered.live/x?apiKey={v}"), "fetch");
            }
        });
        for (i, v) in values.iter().enumerate() {
            assert!(!out.contains(v.as_str()), "secret #{i} leaked into logs");
        }
    }
}
