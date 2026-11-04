//! Connection setup (spec section 5): stores Metered credentials securely, runs the connection
//! test, keeps the plain status file (hostnames + status only) and gates Create/Join.
//!
//! Secrets live only in the secure store (Credential Manager, or a DPAPI file in portable mode)
//! and in memory. Every loaded secret is registered with the log scrubber first.

#![cfg(windows)]

use crate::logging::register_secret;
use crate::paths::AppPaths;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;
use syncwatch_ice_config::metered::{self, ApiCredentials, FetchError, InputError};
use syncwatch_ice_config::probe::{ProbeError, ProbeKind, TestReport};
use syncwatch_ice_config::store::{
    self, DpapiFileStore, KeyringStore, LastGood, SecretStore, StoredCredentials,
};
use syncwatch_ice_config::{IceServer, ParseError, model, parse_ice_servers, test_servers};
use tauri::{AppHandle, Emitter, State};

/// Plain (non-secret) connection settings.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionSettings {
    pub configured: bool,
    /// "apiKey" | "iceServers"
    pub mode: Option<String>,
    /// TURN/STUN host names only (never the Metered app host, never credentials).
    pub hostnames: Vec<String>,
    pub verified: bool,
    pub verified_at_unix: Option<i64>,
    /// "Always use relay (hide my IP address)".
    pub relay_only: bool,
    pub last_results: Vec<ResultSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResultSummary {
    pub label: String,
    pub host: String,
    pub relay: bool,
    pub ok: bool,
    pub latency_ms: Option<u32>,
    pub error: Option<ProbeError>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Warning {
    /// Metered could not be reached; the last verified state is kept.
    MeteredUnreachable,
    /// Fresh credentials could not be fetched; the encrypted last-good copy is used.
    UsingCachedServers,
    /// The secure credential store is unavailable.
    StoreUnavailable,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    #[serde(flatten)]
    pub settings: ConnectionSettings,
    pub warning: Option<Warning>,
    /// Debug builds only: Create/Join allowed without a verified configuration.
    pub dev_bypass: bool,
    /// Create/Join are allowed.
    pub can_connect: bool,
    pub portable: bool,
}

/// Outcome of a Test connection run, for the setup screen.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase", tag = "outcome")]
pub enum TestOutcome {
    /// Input problem before any network access.
    InputError {
        error: InputError,
    },
    ParseError {
        error: String,
        code: &'static str,
    },
    FetchError {
        error: FetchError,
    },
    Tested {
        report: Box<TestReport>,
        saved: bool,
    },
    StoreError,
}

pub struct ConnectionManager {
    store: Box<dyn SecretStore>,
    settings_path: PathBuf,
    last_good: DpapiFileStore,
    settings: Mutex<ConnectionSettings>,
    warning: Mutex<Option<Warning>>,
    portable: bool,
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn parse_code(e: &ParseError) -> &'static str {
    use syncwatch_ice_config::model::ValidationError as V;
    match e {
        ParseError::Empty => "empty",
        ParseError::TooLarge => "tooLarge",
        ParseError::NoArray => "noArray",
        ParseError::Syntax => "syntax",
        ParseError::Invalid(V::Empty) => "noServers",
        ParseError::Invalid(V::TooMany) => "tooMany",
        ParseError::Invalid(V::NoUrls(_)) | ParseError::Invalid(V::BadUrl(_)) => "badUrl",
        ParseError::Invalid(V::BadScheme(_)) => "badScheme",
        ParseError::Invalid(V::MissingCredential(_)) => "missingCredential",
        ParseError::Invalid(V::TooLong(_)) => "tooLong",
    }
}

fn is_network_error(e: Option<ProbeError>) -> bool {
    matches!(
        e,
        Some(ProbeError::Dns | ProbeError::Timeout | ProbeError::Offline | ProbeError::Refused)
    )
}

impl ConnectionManager {
    pub fn new(paths: &AppPaths) -> Self {
        let overflow = DpapiFileStore {
            path: paths.config.join("credentials.dpapi"),
        };
        let store: Box<dyn SecretStore> = if paths.portable {
            Box::new(overflow)
        } else {
            Box::new(KeyringStore {
                account: store::KEYRING_ACCOUNT.into(),
                overflow,
            })
        };
        let settings_path = paths.config.join("connection.json");
        let settings = std::fs::read(&settings_path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let m = Self {
            store,
            settings_path,
            last_good: DpapiFileStore {
                path: paths.cache.join("ice-last-good.dpapi"),
            },
            settings: Mutex::new(settings),
            warning: Mutex::new(None),
            portable: paths.portable,
        };
        // Register secrets with the log scrubber as early as possible.
        if let Ok(Some(c)) = store::load_credentials(m.store.as_ref()) {
            for v in c.secret_values() {
                register_secret(&v);
            }
        }
        m
    }

    fn settings(&self) -> ConnectionSettings {
        self.settings.lock().map(|g| g.clone()).unwrap_or_default()
    }

    fn save_settings(&self, s: ConnectionSettings) {
        if let Ok(json) = serde_json::to_vec_pretty(&s) {
            let tmp = self.settings_path.with_extension("tmp");
            if std::fs::write(&tmp, json).is_ok() {
                let _ = std::fs::rename(&tmp, &self.settings_path);
            }
        }
        if let Ok(mut g) = self.settings.lock() {
            *g = s;
        }
    }

    fn set_warning(&self, w: Option<Warning>) {
        if let Ok(mut g) = self.warning.lock() {
            *g = w;
        }
    }

    pub fn dev_bypass() -> bool {
        cfg!(debug_assertions)
            && std::env::var("SYNCWATCH_DEV_BYPASS_CONNECTION").as_deref() == Ok("1")
    }

    pub fn status(&self) -> Status {
        let settings = self.settings();
        let dev_bypass = Self::dev_bypass();
        Status {
            can_connect: settings.verified || dev_bypass,
            settings,
            warning: self.warning.lock().ok().and_then(|g| *g),
            dev_bypass,
            portable: self.portable,
        }
    }

    fn summaries(r: &TestReport) -> Vec<ResultSummary> {
        r.results
            .iter()
            .map(|u| ResultSummary {
                label: u.label.clone(),
                host: u.host.clone(),
                relay: u.kind == ProbeKind::Relay,
                ok: u.ok,
                latency_ms: u.latency_ms,
                error: u.error,
            })
            .collect()
    }

    fn remember_good(&self, servers: &[IceServer]) {
        let lg = LastGood {
            fetched_at_unix: now_unix(),
            servers: servers.to_vec(),
        };
        if let Ok(json) = serde_json::to_vec(&lg) {
            let _ = self.last_good.save(&json);
        }
    }

    fn record(&self, mode: &str, servers: &[IceServer], report: &TestReport) {
        let mut s = self.settings();
        s.configured = true;
        s.mode = Some(mode.into());
        s.hostnames = model::hostnames(servers);
        s.verified = report.verified;
        if report.verified {
            s.verified_at_unix = Some(now_unix());
        }
        s.last_results = Self::summaries(report);
        self.save_settings(s);
    }

    /// Test (and on success save) app name + API key.
    pub async fn test_api_key(&self, app: &str, key: &str) -> TestOutcome {
        let creds = ApiCredentials {
            app: match metered::validate_app_name(app) {
                Ok(a) => a,
                Err(error) => return TestOutcome::InputError { error },
            },
            key: match metered::validate_key(key) {
                Ok(k) => k,
                Err(error) => return TestOutcome::InputError { error },
            },
        };
        register_secret(&creds.app);
        register_secret(&creds.key);
        let servers = match metered::fetch_ice_servers(&creds).await {
            Ok(s) => s,
            Err(error) => return TestOutcome::FetchError { error },
        };
        self.finish_test(
            "apiKey",
            StoredCredentials::ApiKey {
                app: creds.app,
                key: creds.key,
            },
            servers,
        )
        .await
    }

    /// Test (and on success save) a pasted ICE servers configuration.
    pub async fn test_ice(&self, text: &str) -> TestOutcome {
        let servers = match parse_ice_servers(text) {
            Ok(s) => s,
            Err(e) => {
                return TestOutcome::ParseError {
                    error: e.to_string(),
                    code: parse_code(&e),
                };
            }
        };
        let creds = StoredCredentials::IceServers {
            servers: servers.clone(),
        };
        for v in creds.secret_values() {
            register_secret(&v);
        }
        self.finish_test("iceServers", creds, servers).await
    }

    async fn finish_test(
        &self,
        mode: &str,
        creds: StoredCredentials,
        servers: Vec<IceServer>,
    ) -> TestOutcome {
        let report = test_servers(&servers).await;
        let mut saved = false;
        if report.verified {
            if store::save_credentials(self.store.as_ref(), &creds).is_err() {
                self.set_warning(Some(Warning::StoreUnavailable));
                return TestOutcome::StoreError;
            }
            saved = true;
            self.remember_good(&servers);
            self.record(mode, &servers, &report);
            self.set_warning(None);
        }
        tracing::info!(
            verified = report.verified,
            urls = report.results.len(),
            allocations = report.allocations,
            released = report.allocations_released,
            "connection test finished"
        );
        TestOutcome::Tested {
            report: Box::new(report),
            saved,
        }
    }

    /// Re-verifies the saved configuration (Settings > Test, and in the background at start).
    /// If Metered or the network is unreachable, the last verified state is kept with a warning.
    pub async fn retest(&self) -> Option<TestOutcome> {
        let creds = match store::load_credentials(self.store.as_ref()) {
            Ok(Some(c)) => c,
            Ok(None) => return None,
            Err(_) => {
                self.set_warning(Some(Warning::StoreUnavailable));
                return Some(TestOutcome::StoreError);
            }
        };
        for v in creds.secret_values() {
            register_secret(&v);
        }
        let (mode, servers) = match creds {
            StoredCredentials::ApiKey { app, key } => {
                match metered::fetch_ice_servers(&ApiCredentials { app, key }).await {
                    Ok(s) => ("apiKey", s),
                    Err(FetchError::BadKey) => {
                        let mut s = self.settings();
                        s.verified = false;
                        self.save_settings(s);
                        return Some(TestOutcome::FetchError {
                            error: FetchError::BadKey,
                        });
                    }
                    Err(error) => {
                        // Metered unreachable: keep the last verified state, with a warning.
                        self.set_warning(Some(Warning::MeteredUnreachable));
                        return Some(TestOutcome::FetchError { error });
                    }
                }
            }
            StoredCredentials::IceServers { servers } => ("iceServers", servers),
        };
        let report = test_servers(&servers).await;
        let offline = !report.verified
            && report
                .results
                .iter()
                .all(|r| r.ok || is_network_error(r.error));
        if report.verified {
            self.remember_good(&servers);
            self.record(mode, &servers, &report);
            self.set_warning(None);
        } else if offline && self.settings().verified {
            self.set_warning(Some(Warning::MeteredUnreachable));
        } else {
            self.record(mode, &servers, &report);
        }
        Some(TestOutcome::Tested {
            report: Box::new(report),
            saved: false,
        })
    }

    /// ICE servers for a create/join: fresh from Metered on every call (API-key mode), falling
    /// back to the encrypted last-good copy with a visible warning.
    #[allow(dead_code)] // used by rooms (Milestone 5)
    pub async fn servers_for_session(&self) -> Option<(Vec<IceServer>, Option<Warning>)> {
        match store::load_credentials(self.store.as_ref()).ok()?? {
            StoredCredentials::IceServers { servers } => Some((servers, None)),
            StoredCredentials::ApiKey { app, key } => {
                match metered::fetch_ice_servers(&ApiCredentials { app, key }).await {
                    Ok(s) => {
                        self.remember_good(&s);
                        Some((s, None))
                    }
                    Err(_) => {
                        let lg: LastGood =
                            serde_json::from_slice(&self.last_good.load().ok()??).ok()?;
                        Some((lg.servers, Some(Warning::UsingCachedServers)))
                    }
                }
            }
        }
    }

    pub fn remove(&self) {
        let _ = self.store.delete();
        let _ = self.last_good.delete();
        let relay_only = self.settings().relay_only;
        self.save_settings(ConnectionSettings {
            relay_only,
            ..ConnectionSettings::default()
        });
        self.set_warning(None);
    }

    pub fn set_relay_only(&self, on: bool) {
        let mut s = self.settings();
        s.relay_only = on;
        self.save_settings(s);
    }
}

// ------------------------------------------------------------------------- commands ------

#[tauri::command]
pub fn connection_status(m: State<'_, ConnectionManager>) -> Status {
    m.status()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SplitResult {
    app: String,
    key: String,
}

/// Splits a pasted credentials URL into app name and key (the one-box input). The values only
/// go back to the same input form they came from.
#[tauri::command]
pub fn connection_split_url(url: String) -> Result<SplitResult, InputError> {
    let c = metered::split_credentials_url(&url)?;
    register_secret(&c.app);
    register_secret(&c.key);
    Ok(SplitResult {
        app: c.app,
        key: c.key,
    })
}

#[tauri::command]
pub async fn connection_test_api_key(
    app: AppHandle,
    m: State<'_, ConnectionManager>,
    app_name: String,
    key: String,
) -> Result<TestOutcome, ()> {
    let out = m.test_api_key(&app_name, &key).await;
    let _ = app.emit("connection-status", m.status());
    Ok(out)
}

#[tauri::command]
pub async fn connection_test_ice(
    app: AppHandle,
    m: State<'_, ConnectionManager>,
    text: String,
) -> Result<TestOutcome, ()> {
    let out = m.test_ice(&text).await;
    let _ = app.emit("connection-status", m.status());
    Ok(out)
}

#[tauri::command]
pub async fn connection_retest(
    app: AppHandle,
    m: State<'_, ConnectionManager>,
) -> Result<Option<TestOutcome>, ()> {
    let out = m.retest().await;
    let _ = app.emit("connection-status", m.status());
    Ok(out)
}

#[tauri::command]
pub fn connection_remove(app: AppHandle, m: State<'_, ConnectionManager>) -> Status {
    m.remove();
    let s = m.status();
    let _ = app.emit("connection-status", s.clone());
    s
}

#[tauri::command]
pub fn connection_set_relay_only(
    app: AppHandle,
    m: State<'_, ConnectionManager>,
    on: bool,
) -> Status {
    m.set_relay_only(on);
    let s = m.status();
    let _ = app.emit("connection-status", s.clone());
    s
}

/// The ONLY links the app may open (spec section 5 "strictly allowlisted opener").
pub const HELP_LINKS: &[(&str, &str)] = &[
    ("metered", "https://www.metered.ca/"),
    ("meteredDashboard", "https://dashboard.metered.ca/"),
];

#[tauri::command]
pub fn open_help_link(id: String) -> Result<(), String> {
    let Some((_, url)) = HELP_LINKS.iter().find(|(k, _)| *k == id) else {
        return Err("This link is not allowed.".into());
    };
    open_url(url)
}

fn open_url(url: &str) -> Result<(), String> {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::{HSTRING, w};
    let u = HSTRING::from(url);
    // SAFETY: plain ShellExecute of a constant https URL with valid wide strings.
    let r = unsafe { ShellExecuteW(None, w!("open"), &u, None, None, SW_SHOWNORMAL) };
    if r.0 as usize > 32 {
        Ok(())
    } else {
        Err("Could not open the browser.".into())
    }
}

/// Background re-verification at start.
pub fn spawn_background_retest(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        use tauri::Manager;
        let m = app.state::<ConnectionManager>();
        if m.status().settings.configured {
            let _ = m.retest().await;
            let _ = app.emit("connection-status", m.status());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_links_are_https_metered_only() {
        for (_, url) in HELP_LINKS {
            assert!(url.starts_with("https://"));
            assert!(url.contains(".metered.ca/"));
        }
        assert!(open_help_link("https://evil.invalid".into()).is_err());
        assert!(open_help_link("file".into()).is_err());
    }

    #[test]
    fn status_and_settings_never_carry_secrets() {
        let s = ConnectionSettings {
            configured: true,
            mode: Some("apiKey".into()),
            hostnames: vec!["global.relay.metered.ca".into()],
            verified: true,
            verified_at_unix: Some(1),
            relay_only: false,
            last_results: vec![],
        };
        let json = serde_json::to_string(&s).unwrap();
        // No field that could hold a secret exists in the plain settings file.
        for field in [
            "\"key\"",
            "\"app\"",
            "\"credential\"",
            "\"username\"",
            "\"servers\"",
        ] {
            assert!(!json.contains(field), "{field} in {json}");
        }
    }
}
