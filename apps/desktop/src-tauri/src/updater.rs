//! In-app updates from GitHub Releases (Milestone 9, ADR 0016).
//!
//! 1. After the first paint (never blocking, never during an active room) the UI asks for a
//!    check. The app fetches `latest.json` and `latest.json.sig` from the repository's latest
//!    release: that is the only data sent (a standard HTTPS request to GitHub).
//! 2. The manifest must carry a valid minisign signature from the maintainer's updater key
//!    (Tauri's signer format; only the public key is in the app). It names the version, the
//!    MSI URL, its SHA-256 and the MSI's own signature. Only a strictly newer version is
//!    offered: an old (validly signed) release can never be pushed as an update.
//! 3. "Update now" downloads the MSI with progress and a cancel button, then checks its size,
//!    SHA-256 and signature. Only then "Restart to update" appears.
//! 4. On confirm the app starts the system msiexec (absolute path) with the per-user MSI in
//!    passive mode and quits; the MSI's major upgrade replaces the old version and starts the
//!    app again (AUTOLAUNCHAPP).
//!
//! Network rules: HTTPS only; the first URL and every redirect must be on github.com or a
//! GitHub release-asset host; at most 5 redirects; connect 10 s, manifest 20 s, download read
//! timeout 30 s; manifest at most 64 KiB, MSI at most 600 MiB. No token, no account: the
//! repository is public. Debug builds only: SYNCWATCH_DEV_UPDATE_URL (a local test server on
//! 127.0.0.1) and SYNCWATCH_DEV_UPDATE_PUBKEY (a throwaway test key).

use base64::Engine as _;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use sha2::Digest as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};

const RELEASE_BASE: &str = "https://github.com/ramdn99/syncwatch/releases/latest/download/";
/// Hosts the updater may talk to (the first request and every redirect).
const HOSTS: &[&str] = &[
    "github.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
];
/// The maintainer's updater public key (`tauri signer generate`), base64 as Tauri prints it.
/// Replaced by the real key before the first release (docs/release-guide.md, step a).
const PUBKEY: &str = include_str!("../updater-pubkey.txt");
pub const PLATFORM: &str = "windows-x86_64";
const MAX_REDIRECTS: usize = 5;
const MAX_MANIFEST: usize = 64 * 1024;
const MAX_SIG: usize = 4 * 1024;
const MAX_MSI: u64 = 600 * 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const SMALL_TIMEOUT: Duration = Duration::from_secs(20);
const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// `latest.json` (Tauri's static updater format, plus `sha256` per platform).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Manifest {
    pub version: String,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub pub_date: Option<String>,
    pub platforms: std::collections::BTreeMap<String, PlatformEntry>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PlatformEntry {
    /// Base64 of the minisign signature of the MSI (`tauri signer sign`).
    pub signature: String,
    pub url: String,
    /// Lowercase hex SHA-256 of the MSI.
    pub sha256: String,
}

/// A verified offer: a newer version, its MSI location and how to check it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offer {
    pub version: semver::Version,
    pub notes: String,
    pub url: Url,
    pub sha256: String,
    pub signature: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UpdateError {
    #[error("The update check could not reach GitHub ({0}).")]
    Network(String),
    #[error("GitHub answered with an error ({0}).")]
    Http(u16),
    #[error("The update information is too large or incomplete.")]
    TooLarge,
    #[error("The update information is not signed by the SyncWatch maintainer. It was ignored.")]
    BadSignature,
    #[error("The update information is not valid.")]
    BadManifest,
    #[error("The update is not newer than this version; it was ignored.")]
    NotNewer,
    #[error("The update points to a server that is not allowed.")]
    HostNotAllowed,
    #[error("The downloaded file does not match the update information. It was deleted.")]
    HashMismatch,
    #[error("The download was cancelled.")]
    Cancelled,
    #[error("Updates are not set up in this build (no updater key).")]
    NoKey,
    #[error("Could not save the update: {0}")]
    Io(String),
}

// ---------------------------------------------------------------------------- pure checks ----

/// Debug builds, and test MSIs built with SYNCWATCH_TEST_BUILD=1 (scripts/build-release.ps1
/// -TestBuild, never used for releases), accept a local update server and a test key.
pub const TEST_BUILD: bool =
    cfg!(debug_assertions) || option_env!("SYNCWATCH_TEST_BUILD").is_some();

/// The trusted public key: the compiled-in one, or (test builds only) a test key.
fn public_key() -> Result<minisign_verify::PublicKey, UpdateError> {
    let dev = TEST_BUILD
        .then(|| std::env::var("SYNCWATCH_DEV_UPDATE_PUBKEY").ok())
        .flatten();
    let b64 = dev.as_deref().unwrap_or(PUBKEY).trim();
    decode_pubkey(b64)
}

pub fn decode_pubkey(b64: &str) -> Result<minisign_verify::PublicKey, UpdateError> {
    let text = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .map_err(|_| UpdateError::NoKey)?;
    let text = String::from_utf8(text).map_err(|_| UpdateError::NoKey)?;
    minisign_verify::PublicKey::decode(&text).map_err(|_| UpdateError::NoKey)
}

/// Verifies a Tauri-format signature (base64 of the minisign signature text) over `data`.
pub fn verify(
    pk: &minisign_verify::PublicKey,
    data: &[u8],
    sig_b64: &str,
) -> Result<(), UpdateError> {
    let text = base64::engine::general_purpose::STANDARD
        .decode(sig_b64.trim())
        .map_err(|_| UpdateError::BadSignature)?;
    let text = String::from_utf8(text).map_err(|_| UpdateError::BadSignature)?;
    let sig = minisign_verify::Signature::decode(&text).map_err(|_| UpdateError::BadSignature)?;
    pk.verify(data, &sig, false)
        .map_err(|_| UpdateError::BadSignature)
}

/// Is `url` a place the updater may fetch from (or be redirected to)?
pub fn allowed_url(url: &Url, dev_base: Option<&Url>) -> bool {
    if let Some(base) = dev_base
        && url.scheme() == "http"
        && url.host_str() == base.host_str()
        && url.port() == base.port()
        && matches!(url.host_str(), Some("127.0.0.1" | "localhost"))
    {
        return true;
    }
    url.scheme() == "https"
        && url.port().is_none()
        && url
            .host_str()
            .is_some_and(|h| HOSTS.iter().any(|a| a.eq_ignore_ascii_case(h)))
}

/// Checks a manifest and its signature, and turns it into an offer if it is strictly newer.
pub fn check_manifest(
    pk: &minisign_verify::PublicKey,
    json: &[u8],
    sig_b64: &str,
    current: &semver::Version,
    dev_base: Option<&Url>,
) -> Result<Offer, UpdateError> {
    if json.len() > MAX_MANIFEST || sig_b64.len() > MAX_SIG {
        return Err(UpdateError::TooLarge);
    }
    verify(pk, json, sig_b64)?;
    let m: Manifest = serde_json::from_slice(json).map_err(|_| UpdateError::BadManifest)?;
    let version = semver::Version::parse(m.version.trim_start_matches('v'))
        .map_err(|_| UpdateError::BadManifest)?;
    if &version <= current {
        return Err(UpdateError::NotNewer);
    }
    let p = m.platforms.get(PLATFORM).ok_or(UpdateError::BadManifest)?;
    let url = Url::parse(&p.url).map_err(|_| UpdateError::BadManifest)?;
    if !allowed_url(&url, dev_base) {
        return Err(UpdateError::HostNotAllowed);
    }
    let file = format!("SyncWatch-{version}-x64.msi");
    if !url.path().ends_with(&format!("/{file}")) {
        return Err(UpdateError::BadManifest);
    }
    let sha = p.sha256.to_ascii_lowercase();
    if sha.len() != 64 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(UpdateError::BadManifest);
    }
    let notes: String = m
        .notes
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .take(2000)
        .collect();
    Ok(Offer {
        version,
        notes,
        url,
        sha256: sha,
        signature: p.signature.clone(),
    })
}

/// Checks a downloaded MSI against the offer: SHA-256 and the maintainer's signature.
pub fn check_download(
    pk: &minisign_verify::PublicKey,
    offer: &Offer,
    data: &[u8],
) -> Result<(), UpdateError> {
    let sha = hex(&sha2::Sha256::digest(data));
    if sha != offer.sha256 {
        return Err(UpdateError::HashMismatch);
    }
    verify(pk, data, &offer.signature)
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

// ------------------------------------------------------------------------------ network ------

fn dev_base() -> Option<Url> {
    if !TEST_BUILD {
        return None;
    }
    let u = Url::parse(&std::env::var("SYNCWATCH_DEV_UPDATE_URL").ok()?).ok()?;
    matches!(u.host_str(), Some("127.0.0.1" | "localhost")).then_some(u)
}

fn client(timeout: Option<Duration>, dev: Option<Url>) -> Result<reqwest::Client, UpdateError> {
    // The rustls crypto provider is installed once at startup (commands.rs).
    let loopback = dev.is_some();
    let policy = reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() >= MAX_REDIRECTS || !allowed_url(attempt.url(), dev.as_ref()) {
            attempt.error("redirect target not allowed")
        } else {
            attempt.follow()
        }
    });
    let mut b = reqwest::Client::builder()
        .no_proxy()
        .redirect(policy)
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(READ_TIMEOUT)
        .user_agent(concat!(
            "SyncWatch/",
            env!("CARGO_PKG_VERSION"),
            " (update check)"
        ));
    if !loopback {
        b = b.https_only(true);
    }
    if let Some(t) = timeout {
        b = b.timeout(t);
    }
    b.build().map_err(|e| UpdateError::Network(e.to_string()))
}

fn base() -> Url {
    dev_base().unwrap_or_else(|| {
        // A compile-time constant: parsing cannot fail.
        Url::parse(RELEASE_BASE).unwrap_or_else(|_| unreachable!())
    })
}

async fn get_small(
    c: &reqwest::Client,
    url: Url,
    max: usize,
    dev: Option<&Url>,
) -> Result<Vec<u8>, UpdateError> {
    if !allowed_url(&url, dev) {
        return Err(UpdateError::HostNotAllowed);
    }
    let mut resp = c.get(url).send().await.map_err(net)?;
    if !resp.status().is_success() {
        return Err(UpdateError::Http(resp.status().as_u16()));
    }
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(net)? {
        if body.len() + chunk.len() > max {
            return Err(UpdateError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn net(e: reqwest::Error) -> UpdateError {
    if e.is_redirect() {
        UpdateError::HostNotAllowed
    } else if e.is_timeout() {
        UpdateError::Network("timed out".into())
    } else if e.is_connect() {
        UpdateError::Network("no connection".into())
    } else {
        UpdateError::Network("request failed".into())
    }
}

/// Fetches and checks the latest manifest.
pub async fn check(current: &semver::Version) -> Result<Offer, UpdateError> {
    check_at(&base(), dev_base().as_ref(), &public_key()?, current).await
}

/// `check` against a given release base URL (tests use a local server).
pub async fn check_at(
    base: &Url,
    dev: Option<&Url>,
    pk: &minisign_verify::PublicKey,
    current: &semver::Version,
) -> Result<Offer, UpdateError> {
    let c = client(Some(SMALL_TIMEOUT), dev.cloned())?;
    let json_url = base
        .join("latest.json")
        .map_err(|_| UpdateError::BadManifest)?;
    let sig_url = base
        .join("latest.json.sig")
        .map_err(|_| UpdateError::BadManifest)?;
    let json = get_small(&c, json_url, MAX_MANIFEST, dev).await?;
    let sig = match get_small(&c, sig_url, MAX_SIG, dev).await {
        // No signature published: an unsigned release, refused.
        Err(UpdateError::Http(404)) => return Err(UpdateError::BadSignature),
        other => other?,
    };
    let sig = String::from_utf8(sig).map_err(|_| UpdateError::BadSignature)?;
    check_manifest(pk, &json, &sig, current, dev)
}

/// Downloads the MSI to `dir`, reporting progress, and verifies it. Returns the file path.
pub async fn download(
    offer: &Offer,
    dir: &Path,
    cancel: &AtomicBool,
    progress: impl FnMut(u64, Option<u64>),
) -> Result<PathBuf, UpdateError> {
    download_with(
        offer,
        dir,
        cancel,
        progress,
        &public_key()?,
        dev_base().as_ref(),
    )
    .await
}

/// `download` with an explicit key and test server (tests).
pub async fn download_with(
    offer: &Offer,
    dir: &Path,
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64, Option<u64>),
    pk: &minisign_verify::PublicKey,
    dev: Option<&Url>,
) -> Result<PathBuf, UpdateError> {
    if !allowed_url(&offer.url, dev) {
        return Err(UpdateError::HostNotAllowed);
    }
    let c = client(None, dev.cloned())?;
    let mut resp = c.get(offer.url.clone()).send().await.map_err(net)?;
    if !resp.status().is_success() {
        return Err(UpdateError::Http(resp.status().as_u16()));
    }
    let total = resp.content_length();
    if total.is_some_and(|t| t > MAX_MSI) {
        return Err(UpdateError::TooLarge);
    }
    let mut data: Vec<u8> = Vec::with_capacity(total.unwrap_or(0).min(MAX_MSI) as usize);
    while let Some(chunk) = resp.chunk().await.map_err(net)? {
        if cancel.load(Ordering::SeqCst) {
            return Err(UpdateError::Cancelled);
        }
        if data.len() as u64 + chunk.len() as u64 > MAX_MSI {
            return Err(UpdateError::TooLarge);
        }
        data.extend_from_slice(&chunk);
        progress(data.len() as u64, total);
    }
    check_download(pk, offer, &data)?;
    std::fs::create_dir_all(dir).map_err(|e| UpdateError::Io(e.to_string()))?;
    let path = dir.join(format!("SyncWatch-{}-x64.msi", offer.version));
    std::fs::write(&path, &data).map_err(|e| UpdateError::Io(e.to_string()))?;
    Ok(path)
}

// -------------------------------------------------------------------------- app plumbing -----

/// What the UI shows.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateState {
    /// idle, checking, upToDate, available, downloading, ready, failed.
    pub phase: &'static str,
    pub version: Option<String>,
    pub notes: Option<String>,
    pub received: u64,
    pub total: Option<u64>,
    pub error: Option<String>,
}

pub struct UpdateService {
    /// Where a downloaded update waits (the app's cache folder).
    dir: PathBuf,
    state: Mutex<UpdateState>,
    offer: Mutex<Option<Offer>>,
    ready: Mutex<Option<PathBuf>>,
    cancel: AtomicBool,
}

impl UpdateService {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            state: Mutex::new(UpdateState {
                phase: "idle",
                ..Default::default()
            }),
            offer: Mutex::new(None),
            ready: Mutex::new(None),
            cancel: AtomicBool::new(false),
        }
    }

    fn set(&self, app: &AppHandle, f: impl FnOnce(&mut UpdateState)) {
        let snapshot = {
            let Ok(mut g) = self.state.lock() else { return };
            f(&mut g);
            g.clone()
        };
        let _ = app.emit("update-state", &snapshot);
    }
}

fn current_version(app: &AppHandle) -> semver::Version {
    semver::Version::parse(&app.package_info().version.to_string())
        .unwrap_or_else(|_| semver::Version::new(0, 0, 0))
}

#[tauri::command]
pub fn update_state(s: tauri::State<'_, UpdateService>) -> UpdateState {
    s.state.lock().map(|g| g.clone()).unwrap_or_default()
}

/// Checks GitHub for a newer release (the UI calls it after the first paint when the setting
/// is on, or from Settings > Check now).
#[tauri::command]
pub async fn update_check(app: AppHandle) -> UpdateState {
    let s = app.state::<UpdateService>();
    s.set(&app, |st| {
        *st = UpdateState {
            phase: "checking",
            ..Default::default()
        }
    });
    let res = check(&current_version(&app)).await;
    match res {
        Ok(offer) => {
            tracing::info!(version = %offer.version, "update available");
            let (v, n) = (offer.version.to_string(), offer.notes.clone());
            if let Ok(mut g) = s.offer.lock() {
                *g = Some(offer);
            }
            s.set(&app, |st| {
                st.phase = "available";
                st.version = Some(v);
                st.notes = Some(n);
            });
        }
        Err(UpdateError::NotNewer) => s.set(&app, |st| st.phase = "upToDate"),
        Err(e) => {
            tracing::info!(error = %e, "update check failed");
            s.set(&app, |st| {
                st.phase = "failed";
                st.error = Some(e.to_string());
            });
        }
    }
    update_state(app.state::<UpdateService>())
}

/// Downloads and verifies the offered MSI (progress through "update-state" events).
#[tauri::command]
pub async fn update_download(app: AppHandle) -> UpdateState {
    let s = app.state::<UpdateService>();
    let Some(offer) = s.offer.lock().ok().and_then(|g| g.clone()) else {
        return update_state(app.state::<UpdateService>());
    };
    s.cancel.store(false, Ordering::SeqCst);
    s.set(&app, |st| {
        st.phase = "downloading";
        st.received = 0;
        st.total = None;
        st.error = None;
    });
    let dir = s.dir.clone();
    let mut last = std::time::Instant::now();
    let app2 = app.clone();
    let res = download(&offer, &dir, &s.cancel, |received, total| {
        if last.elapsed() >= Duration::from_millis(200) {
            last = std::time::Instant::now();
            app2.state::<UpdateService>().set(&app2, |st| {
                st.received = received;
                st.total = total;
            });
        }
    })
    .await;
    match res {
        Ok(path) => {
            tracing::info!(version = %offer.version, "update downloaded and verified");
            if let Ok(mut g) = s.ready.lock() {
                *g = Some(path);
            }
            s.set(&app, |st| st.phase = "ready");
        }
        Err(e) => {
            tracing::warn!(error = %e, "update download failed");
            let _ = std::fs::remove_dir_all(&dir);
            s.set(&app, |st| {
                st.phase = if e == UpdateError::Cancelled {
                    "available"
                } else {
                    "failed"
                };
                st.error = (e != UpdateError::Cancelled).then(|| e.to_string());
            });
        }
    }
    update_state(app.state::<UpdateService>())
}

#[tauri::command]
pub fn update_cancel(s: tauri::State<'_, UpdateService>) {
    s.cancel.store(true, Ordering::SeqCst);
}

/// "Restart to update": checks the verified file once more, starts the system msiexec with the
/// per-user MSI (passive, starts SyncWatch again when done) and quits.
#[tauri::command]
pub fn update_install(app: AppHandle) -> Result<(), String> {
    let s = app.state::<UpdateService>();
    let path = s
        .ready
        .lock()
        .ok()
        .and_then(|g| g.clone())
        .ok_or("No update is ready.")?;
    let offer = s
        .offer
        .lock()
        .ok()
        .and_then(|g| g.clone())
        .ok_or("No update is ready.")?;
    let data = std::fs::read(&path).map_err(|e| e.to_string())?;
    let pk = public_key().map_err(|e| e.to_string())?;
    check_download(&pk, &offer, &data).map_err(|e| e.to_string())?;
    let sysroot = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    let msiexec = Path::new(&sysroot).join("System32").join("msiexec.exe");
    let mut cmd = std::process::Command::new(msiexec);
    cmd.arg("/i")
        .arg(&path)
        .args(["/passive", "AUTOLAUNCHAPP=True"]);
    // Test builds only (Windows Sandbox test): a verbose log of the upgrade.
    if TEST_BUILD && let Some(log) = std::env::var_os("SYNCWATCH_TEST_MSI_LOG") {
        cmd.arg("/l*v").arg(log);
    }
    cmd.spawn()
        .map_err(|e| format!("Could not start the installer: {e}"))?;
    tracing::info!(version = %offer.version, "starting the update installer; quitting");
    app.exit(0);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // A throwaway test key pair made with `tauri signer generate` (password: "test"). Only
    // the public key and two signatures made with it are here; the private key was discarded.
    // See tests/updater-fixtures/README.md.
    const TEST_PUB: &str = include_str!("../tests/updater-fixtures/test.key.pub");

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/updater-fixtures")
                .join(name),
        )
        .unwrap()
    }

    fn pk() -> minisign_verify::PublicKey {
        decode_pubkey(TEST_PUB).unwrap()
    }

    fn v(s: &str) -> semver::Version {
        semver::Version::parse(s).unwrap()
    }

    #[test]
    fn mutated_manifests_and_signatures_are_refused_without_panics() {
        // Fuzz-style (Milestone 9 security review): any change to the signed manifest or its
        // signature must be refused; nothing may panic.
        let json = fixture("latest.json");
        let sig = String::from_utf8(fixture("latest.json.sig")).unwrap();
        let mut s = 0x7570_6461_7465_u64;
        let mut next = || {
            s ^= s >> 12;
            s ^= s << 25;
            s ^= s >> 27;
            s.wrapping_mul(0x2545_f491_4f6c_dd1d)
        };
        for i in 0..3000 {
            let (mut j, mut g) = (json.clone(), sig.clone().into_bytes());
            let target = if i % 2 == 0 { &mut j } else { &mut g };
            let at = (next() % target.len() as u64) as usize;
            match next() % 3 {
                0 => target[at] ^= 1 << (next() % 8),
                1 => target.insert(at, next() as u8),
                _ => {
                    target.remove(at);
                }
            }
            let g = String::from_utf8_lossy(&g).into_owned();
            // A flip inside base64 padding or trailing whitespace can leave the signature
            // bytes unchanged; then the manifest is the original and may be offered.
            if j == json && g.trim() == sig.trim() {
                continue;
            }
            let r = check_manifest(&pk(), &j, &g, &v("0.1.0"), None);
            if j != json {
                assert!(r.is_err(), "changed manifest accepted (round {i})");
            }
        }
    }

    #[test]
    fn a_signed_newer_manifest_is_offered() {
        let json = fixture("latest.json");
        let sig = String::from_utf8(fixture("latest.json.sig")).unwrap();
        let o = check_manifest(&pk(), &json, &sig, &v("0.1.0"), None).unwrap();
        assert_eq!(o.version, v("0.2.0"));
        assert!(o.url.as_str().ends_with("/SyncWatch-0.2.0-x64.msi"));
    }

    #[test]
    fn not_newer_unsigned_tampered_or_wrongly_signed_manifests_are_refused() {
        let json = fixture("latest.json");
        let sig = String::from_utf8(fixture("latest.json.sig")).unwrap();
        // Same or newer installed: no downgrade, no reinstall.
        assert_eq!(
            check_manifest(&pk(), &json, &sig, &v("0.2.0"), None),
            Err(UpdateError::NotNewer)
        );
        assert_eq!(
            check_manifest(&pk(), &json, &sig, &v("0.3.0"), None),
            Err(UpdateError::NotNewer)
        );
        // Unsigned.
        assert_eq!(
            check_manifest(&pk(), &json, "", &v("0.1.0"), None),
            Err(UpdateError::BadSignature)
        );
        // Tampered (one byte changed: the version).
        let tampered = String::from_utf8(json.clone())
            .unwrap()
            .replace("0.2.0", "9.2.0");
        assert_eq!(
            check_manifest(&pk(), tampered.as_bytes(), &sig, &v("0.1.0"), None),
            Err(UpdateError::BadSignature)
        );
        // Signed by another key (the MSI signature is over different data, so it fails too).
        let msi_sig = String::from_utf8(fixture("SyncWatch-0.2.0-x64.msi.sig")).unwrap();
        assert_eq!(
            check_manifest(&pk(), &json, &msi_sig, &v("0.1.0"), None),
            Err(UpdateError::BadSignature)
        );
        // Too large.
        assert_eq!(
            check_manifest(
                &pk(),
                &vec![b' '; MAX_MANIFEST + 1],
                &sig,
                &v("0.1.0"),
                None
            ),
            Err(UpdateError::TooLarge)
        );
    }

    #[test]
    fn downloads_are_checked_by_hash_and_signature() {
        let json = fixture("latest.json");
        let sig = String::from_utf8(fixture("latest.json.sig")).unwrap();
        let o = check_manifest(&pk(), &json, &sig, &v("0.1.0"), None).unwrap();
        let msi = fixture("SyncWatch-0.2.0-x64.msi");
        assert_eq!(check_download(&pk(), &o, &msi), Ok(()));
        let mut bad = msi.clone();
        bad[0] ^= 1;
        assert_eq!(
            check_download(&pk(), &o, &bad),
            Err(UpdateError::HashMismatch)
        );
        // Right hash, wrong signature.
        let o2 = Offer {
            signature: sig.clone(),
            ..o
        };
        assert_eq!(
            check_download(&pk(), &o2, &msi),
            Err(UpdateError::BadSignature)
        );
    }

    #[test]
    fn only_github_hosts_over_https() {
        let ok = |s: &str| allowed_url(&Url::parse(s).unwrap(), None);
        assert!(ok(
            "https://github.com/ramdn99/syncwatch/releases/latest/download/latest.json"
        ));
        assert!(ok("https://objects.githubusercontent.com/x"));
        assert!(ok("https://release-assets.githubusercontent.com/x"));
        assert!(!ok("http://github.com/x"));
        assert!(!ok("https://github.com.evil.example/x"));
        assert!(!ok("https://evil.example/github.com"));
        assert!(!ok("https://github.com:8443/x"));
        assert!(
            !ok("http://127.0.0.1:8080/latest.json"),
            "loopback only with a dev base"
        );
        let dev = Url::parse("http://127.0.0.1:8080/").unwrap();
        assert!(allowed_url(
            &Url::parse("http://127.0.0.1:8080/latest.json").unwrap(),
            Some(&dev)
        ));
        assert!(!allowed_url(
            &Url::parse("http://127.0.0.1:9999/x").unwrap(),
            Some(&dev)
        ));
    }

    #[test]
    fn the_compiled_in_key_is_a_valid_key_or_the_placeholder() {
        // Until the maintainer generates the real key, the placeholder makes updates "not set up".
        let k = PUBKEY.trim();
        assert!(k == "REPLACE_WITH_UPDATER_PUBLIC_KEY" || decode_pubkey(k).is_ok());
    }
}

/// The real network path (reqwest, redirects, allowlist, size and signature checks) against a
/// local update server on 127.0.0.1:18093 serving signed test fixtures.
#[cfg(test)]
mod server_tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    const PORT: u16 = 18093;

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/updater-fixtures")
                .join(name),
        )
        .unwrap()
    }

    /// A route: path, status, body, optional Location header.
    type Route = (String, u16, Vec<u8>, Option<String>);

    /// Serves `routes` on PORT until the test ends (one thread; connections are short).
    fn serve(routes: Vec<Route>) -> std::thread::JoinHandle<()> {
        let l =
            TcpListener::bind(("127.0.0.1", PORT)).expect("port 18093 must be free for this test");
        std::thread::spawn(move || {
            for s in l.incoming().flatten().take(64) {
                let mut s = s;
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut first = String::new();
                let _ = r.read_line(&mut first);
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
                        break;
                    }
                }
                let path = first.split_whitespace().nth(1).unwrap_or("/").to_owned();
                if path == "/stop" {
                    break;
                }
                let (status, body, loc) = routes
                    .iter()
                    .find(|r| r.0 == path)
                    .map(|r| (r.1, r.2.clone(), r.3.clone()))
                    .unwrap_or((404, Vec::new(), None));
                let mut head = format!(
                    "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n",
                    body.len()
                );
                if let Some(l) = loc {
                    head.push_str(&format!("Location: {l}\r\n"));
                }
                head.push_str("\r\n");
                let _ = s.write_all(head.as_bytes());
                let _ = s.write_all(&body);
            }
        })
    }

    fn stop() {
        let _ = std::net::TcpStream::connect(("127.0.0.1", PORT))
            .and_then(|mut s| s.write_all(b"GET /stop HTTP/1.1\r\n\r\n"));
    }

    fn rt() -> tokio::runtime::Runtime {
        // Tests have no app startup: install the rustls provider here.
        syncwatch_ice_config::install_crypto_provider();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    fn base() -> Url {
        Url::parse(&format!("http://127.0.0.1:{PORT}/")).unwrap()
    }

    /// One test (the port is shared): newer, older, unsigned, tampered, wrong key, bad redirect.
    #[test]
    fn local_server_newer_older_unsigned_tampered_and_redirects() {
        let pk = decode_pubkey(&String::from_utf8(fixture("test.key.pub")).unwrap()).unwrap();
        let other = decode_pubkey(&String::from_utf8(fixture("other.key.pub")).unwrap()).unwrap();
        let msi = fixture("SyncWatch-0.2.0-x64.msi");
        let mut tampered = msi.clone();
        tampered[100] ^= 0xff;
        let r = |p: &str, b: Vec<u8>| (p.to_owned(), 200, b, None);
        let routes = vec![
            // /new: a newer signed release
            r("/new/latest.json", fixture("latest-local.json")),
            r("/new/latest.json.sig", fixture("latest-local.json.sig")),
            r("/SyncWatch-0.2.0-x64.msi", msi.clone()),
            // /old: an older signed release
            r("/old/latest.json", fixture("latest-old.json")),
            r("/old/latest.json.sig", fixture("latest-old.json.sig")),
            // /unsigned: no signature published
            r("/unsigned/latest.json", fixture("latest-local.json")),
            // /redirect: the manifest is redirected to a host that is not allowed
            (
                "/redirect/latest.json".to_owned(),
                302,
                Vec::new(),
                Some("https://evil.example/latest.json".to_owned()),
            ),
        ];
        let server = serve(routes);
        let rt = rt();
        let dev = base();
        let cur = semver::Version::new(0, 1, 0);
        let at = |p: &str| base().join(p).unwrap();

        // Newer: offered, downloaded and verified.
        let offer = rt
            .block_on(check_at(&at("new/"), Some(&dev), &pk, &cur))
            .expect("newer release offered");
        assert_eq!(offer.version, semver::Version::new(0, 2, 0));
        let dir = std::env::temp_dir().join(format!("sw-upd-test-{}", std::process::id()));
        let cancel = AtomicBool::new(false);
        let mut seen = 0;
        let path = rt
            .block_on(download_with(
                &offer,
                &dir,
                &cancel,
                |r, _| seen = r,
                &pk,
                Some(&dev),
            ))
            .expect("download verified");
        assert_eq!(std::fs::read(&path).unwrap(), msi);
        assert_eq!(seen, msi.len() as u64, "progress reported");
        // The same offer verified with another key: refused (wrong signer).
        assert_eq!(
            rt.block_on(download_with(
                &offer,
                &dir,
                &cancel,
                |_, _| {},
                &other,
                Some(&dev)
            )),
            Err(UpdateError::BadSignature)
        );
        // Cancelled downloads stop.
        cancel.store(true, Ordering::SeqCst);
        assert_eq!(
            rt.block_on(download_with(
                &offer,
                &dir,
                &cancel,
                |_, _| {},
                &pk,
                Some(&dev)
            )),
            Err(UpdateError::Cancelled)
        );
        cancel.store(false, Ordering::SeqCst);
        // Older: never offered (no downgrade).
        assert_eq!(
            rt.block_on(check_at(&at("old/"), Some(&dev), &pk, &cur)),
            Err(UpdateError::NotNewer)
        );
        // Unsigned: refused.
        assert_eq!(
            rt.block_on(check_at(&at("unsigned/"), Some(&dev), &pk, &cur)),
            Err(UpdateError::BadSignature)
        );
        // A manifest signed by the right key but checked against another: refused.
        assert_eq!(
            rt.block_on(check_at(&at("new/"), Some(&dev), &other, &cur)),
            Err(UpdateError::BadSignature)
        );
        // Redirect to a host that is not allowed: refused before any request goes there.
        assert_eq!(
            rt.block_on(check_at(&at("redirect/"), Some(&dev), &pk, &cur)),
            Err(UpdateError::HostNotAllowed)
        );
        // Tampered MSI: refused.
        let tampered_check = check_download(&pk, &offer, &tampered);
        assert_eq!(tampered_check, Err(UpdateError::HashMismatch));
        // Without the test server, a loopback URL is not allowed at all (release behaviour).
        assert!(!allowed_url(&at("new/latest.json"), None));
        stop();
        let _ = server.join();
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Test builds only (`SYNCWATCH_TEST_BUILD`, never releases): with
/// `SYNCWATCH_TEST_AUTO_UPDATE=1` the app checks, downloads and installs an update by itself,
/// so the clean-machine test can prove the whole upgrade path without any input.
pub fn test_auto_update(app: &AppHandle) {
    if !TEST_BUILD || std::env::var("SYNCWATCH_TEST_AUTO_UPDATE").as_deref() != Ok("1") {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(3)).await;
        let s = update_check(app.clone()).await;
        tracing::info!(phase = s.phase, "test auto-update: check");
        if s.phase != "available" {
            return;
        }
        let s = update_download(app.clone()).await;
        tracing::info!(phase = s.phase, "test auto-update: download");
        if s.phase == "ready"
            && let Err(e) = update_install(app.clone())
        {
            tracing::warn!(error = %e, "test auto-update: install failed");
        }
    });
}
