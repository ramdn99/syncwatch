//! Metered credentials (spec section 5, Tab A).
//!
//! The API key is sent ONLY to `https://<app>.metered.live/api/v1/turn/credentials`. One allowlist
//! (`is_allowed_host`) is used for the initial URL and for every redirect hop: HTTPS only,
//! `<app>.metered.live` or `*.metered.ca`, no userinfo, no port other than 443, no look-alike
//! domains. Errors never contain the URL (it holds the key).

use crate::model::IceServer;
use crate::parse::{ParseError, validate_list};
use reqwest::Url;
use std::time::Duration;

/// The ONLY places a Metered API key may be sent.
pub const ALLOWED_APEX: &[&str] = &["metered.live", "metered.ca"];
pub const CREDENTIALS_PATH: &str = "/api/v1/turn/credentials";
/// Metered's credentials API answered in 5.1 s during the Milestone 9 two-instance test, and
/// the old 8 s limit made room creation fall back to cached servers on the testers' PCs. One
/// attempt may take this long; a timeout or network error is retried once (fetch_ice_servers).
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(20);
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
pub const MAX_BODY_BYTES: usize = 64 * 1024;
const MAX_REDIRECTS: usize = 3;

/// App name + API key. `Debug` is redacted.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ApiCredentials {
    pub app: String,
    pub key: String,
}

impl std::fmt::Debug for ApiCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApiCredentials { app: <app-name>, key: <API_KEY> }")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum InputError {
    #[error("app name is invalid")]
    BadAppName,
    #[error("API key is invalid")]
    BadKey,
    #[error("not a Metered credentials URL")]
    BadUrl,
}

/// `<app>` label rules: one DNS label of letters, digits, `-` and `_` (Metered app names may
/// contain underscores; DNS allows them even though RFC 1123 host names do not).
pub fn validate_app_name(app: &str) -> Result<String, InputError> {
    let a = app.trim().to_ascii_lowercase();
    let ok = !a.is_empty()
        && a.len() <= 63
        && a.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        && !a.starts_with(['-', '_'])
        && !a.ends_with(['-', '_']);
    if ok {
        Ok(a)
    } else {
        Err(InputError::BadAppName)
    }
}

/// API keys: 8-256 visible ASCII characters, no URL delimiters or whitespace.
pub fn validate_key(key: &str) -> Result<String, InputError> {
    let k = key.trim();
    let ok = (8..=256).contains(&k.len())
        && k.chars().all(|c| {
            c.is_ascii_graphic()
                && !matches!(c, '&' | '#' | '?' | '/' | '"' | '\'' | '<' | '>' | '\\')
        });
    if ok {
        Ok(k.to_owned())
    } else {
        Err(InputError::BadKey)
    }
}

/// Is `url` a destination the key may be sent to (or redirected to)?
/// `app` pins `<app>.metered.live`; any `*.metered.ca` host is allowed.
pub fn is_allowed_host(url: &Url, app: &str) -> bool {
    if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    if url.port().is_some_and(|p| p != 443) {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    let host = host.to_ascii_lowercase();
    if host == format!("{app}.metered.live") {
        return true;
    }
    host.strip_suffix(".metered.ca").is_some_and(|sub| {
        !sub.is_empty()
            && sub
                .split('.')
                .all(|l| !l.is_empty() && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
    })
}

/// Builds the credentials URL. The key goes in the query (as Metered documents it).
pub fn credentials_url(c: &ApiCredentials) -> Result<Url, InputError> {
    let app = validate_app_name(&c.app)?;
    let key = validate_key(&c.key)?;
    let mut url = Url::parse(&format!("https://{app}.metered.live{CREDENTIALS_PATH}"))
        .map_err(|_| InputError::BadAppName)?;
    url.query_pairs_mut().append_pair("apiKey", &key);
    if !is_allowed_host(&url, &app) {
        return Err(InputError::BadAppName);
    }
    Ok(url)
}

/// Splits a pasted credentials URL into app name and key (the one-box input).
pub fn split_credentials_url(text: &str) -> Result<ApiCredentials, InputError> {
    let url = Url::parse(text.trim()).map_err(|_| InputError::BadUrl)?;
    let host = url
        .host_str()
        .ok_or(InputError::BadUrl)?
        .to_ascii_lowercase();
    let app = host
        .strip_suffix(".metered.live")
        .ok_or(InputError::BadUrl)?;
    let app = validate_app_name(app)?;
    if !is_allowed_host(&url, &app) || url.path() != CREDENTIALS_PATH {
        return Err(InputError::BadUrl);
    }
    let key = url
        .query_pairs()
        .find(|(k, _)| k.eq_ignore_ascii_case("apiKey"))
        .map(|(_, v)| v.into_owned())
        .ok_or(InputError::BadKey)?;
    Ok(ApiCredentials {
        app,
        key: validate_key(&key)?,
    })
}

/// Fetch failures, mapped to friendly messages in the UI (`connection-help.ts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error, serde::Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum FetchError {
    #[error("invalid input")]
    Input { error: InputError },
    #[error("Metered rejected the API key")]
    BadKey,
    #[error("Metered returned an error (HTTP {status})")]
    Server { status: u16 },
    #[error("the Metered host name could not be resolved")]
    Dns,
    #[error("the connection to Metered was refused")]
    Refused,
    #[error("Metered did not answer in time")]
    Timeout,
    #[error("the secure connection to Metered failed")]
    Tls,
    #[error("no network connection")]
    Offline,
    #[error("Metered sent an unexpected answer")]
    BadResponse,
    #[error("Metered sent a redirect to a host that is not allowed")]
    BadRedirect,
}

/// Classifies a reqwest error WITHOUT keeping any part of it (its Display includes the URL).
fn classify(e: &reqwest::Error) -> FetchError {
    if e.is_redirect() {
        return FetchError::BadRedirect;
    }
    if e.is_timeout() {
        return FetchError::Timeout;
    }
    let mut src: Option<&dyn std::error::Error> = Some(e);
    while let Some(s) = src {
        if let Some(io) = s.downcast_ref::<std::io::Error>() {
            match io.kind() {
                std::io::ErrorKind::ConnectionRefused => return FetchError::Refused,
                std::io::ErrorKind::TimedOut => return FetchError::Timeout,
                std::io::ErrorKind::NetworkUnreachable | std::io::ErrorKind::HostUnreachable => {
                    return FetchError::Offline;
                }
                _ => {}
            }
        }
        let text = s.to_string().to_ascii_lowercase();
        if text.contains("dns error")
            || text.contains("failed to lookup")
            || text.contains("no such host")
        {
            return FetchError::Dns;
        }
        if text.contains("certificate") || text.contains("tls") || text.contains("handshake") {
            return FetchError::Tls;
        }
        src = s.source();
    }
    if e.is_connect() {
        FetchError::Offline
    } else {
        FetchError::BadResponse
    }
}

/// Interprets the HTTP status and body (body text first, spec section 1).
pub fn interpret_response(status: u16, body: &[u8]) -> Result<Vec<IceServer>, FetchError> {
    let text = String::from_utf8_lossy(body);
    if text.contains("Invalid API Key") {
        return Err(FetchError::BadKey);
    }
    if status == 401 || status == 403 {
        return Err(FetchError::BadKey);
    }
    if !(200..300).contains(&status) {
        return Err(FetchError::Server { status });
    }
    let v: serde_json::Value = serde_json::from_slice(body).map_err(|_| FetchError::BadResponse)?;
    let list = match v {
        serde_json::Value::Array(a) => a,
        serde_json::Value::Object(mut o) => match o.remove("iceServers") {
            Some(serde_json::Value::Array(a)) => a,
            _ => return Err(FetchError::BadResponse),
        },
        _ => return Err(FetchError::BadResponse),
    };
    validate_list(list).map_err(|e: ParseError| {
        tracing::warn!(error = %e, "Metered response failed validation");
        FetchError::BadResponse
    })
}

/// Fetches fresh ICE servers from Metered.
/// Fetches fresh ICE servers; a timeout or network error is retried once. Logs the duration
/// and the error class (never the URL, app name or key).
pub async fn fetch_ice_servers(c: &ApiCredentials) -> Result<Vec<IceServer>, FetchError> {
    let t0 = std::time::Instant::now();
    let mut r = fetch_once(c).await;
    if matches!(
        r,
        Err(FetchError::Timeout | FetchError::Refused | FetchError::Offline | FetchError::Dns)
    ) {
        tracing::warn!(error = ?r.as_ref().err(), ms = t0.elapsed().as_millis() as u64, "Metered credentials fetch failed; retrying once");
        r = fetch_once(c).await;
    }
    match &r {
        Ok(s) => tracing::info!(
            servers = s.len(),
            ms = t0.elapsed().as_millis() as u64,
            "Metered credentials fetched"
        ),
        Err(e) => {
            tracing::warn!(error = ?e, ms = t0.elapsed().as_millis() as u64, "Metered credentials fetch failed")
        }
    }
    r
}

async fn fetch_once(c: &ApiCredentials) -> Result<Vec<IceServer>, FetchError> {
    let url = credentials_url(c).map_err(|error| FetchError::Input { error })?;
    let app = validate_app_name(&c.app).map_err(|error| FetchError::Input { error })?;
    let policy = reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() >= MAX_REDIRECTS || !is_allowed_host(attempt.url(), &app) {
            attempt.error("redirect target not allowed")
        } else {
            attempt.follow()
        }
    });
    let client = reqwest::Client::builder()
        .https_only(true)
        .no_proxy()
        .redirect(policy)
        .timeout(FETCH_TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT)
        .user_agent(concat!("SyncWatch/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|_| FetchError::Tls)?;
    let mut resp = client.get(url).send().await.map_err(|e| classify(&e))?;
    let status = resp.status().as_u16();
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| classify(&e))? {
        if body.len() + chunk.len() > MAX_BODY_BYTES {
            return Err(FetchError::BadResponse);
        }
        body.extend_from_slice(&chunk);
    }
    interpret_response(status, &body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn allowlist_accepts_only_metered_hosts() {
        assert!(is_allowed_host(
            &url("https://myapp.metered.live/api/v1/turn/credentials"),
            "myapp"
        ));
        assert!(is_allowed_host(
            &url("https://myapp.metered.live:443/x"),
            "myapp"
        ));
        assert!(is_allowed_host(
            &url("https://dashboard.metered.ca/x"),
            "myapp"
        ));
        for bad in [
            "http://myapp.metered.live/x",
            "https://other.metered.live/x",
            "https://myapp.metered.live:8443/x",
            "https://user:pw@myapp.metered.live/x",
            "https://myapp.metered.live.evil.invalid/x",
            "https://evilmetered.ca/x",
            "https://metered.ca.evil.invalid/x",
            "https://myapp-metered.live/x",
            "https://xn--mtered-6ua.ca/x",
        ] {
            assert!(!is_allowed_host(&url(bad), "myapp"), "{bad}");
        }
    }

    #[test]
    fn split_and_build_round_trip() {
        let c = split_credentials_url(
            "https://MyApp.metered.live/api/v1/turn/credentials?apiKey=placeholder-key",
        )
        .unwrap();
        assert_eq!(c.app, "myapp");
        assert_eq!(c.key, "placeholder-key");
        let u = credentials_url(&c).unwrap();
        assert_eq!(
            u.as_str(),
            "https://myapp.metered.live/api/v1/turn/credentials?apiKey=placeholder-key"
        );
        for bad in [
            "https://myapp.metered.live/other?apiKey=placeholder-key",
            "https://myapp.metered.live.evil.invalid/api/v1/turn/credentials?apiKey=placeholder-key",
            "http://myapp.metered.live/api/v1/turn/credentials?apiKey=placeholder-key",
            "https://myapp.metered.live/api/v1/turn/credentials",
            "not a url",
        ] {
            assert!(split_credentials_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn input_validation() {
        assert!(validate_app_name("my-app1").is_ok());
        assert!(
            validate_app_name("my_app1").is_ok(),
            "Metered app names may contain underscores"
        );
        assert!(validate_app_name("_x").is_err());
        assert!(validate_app_name("my.app").is_err());
        assert!(validate_app_name("-x").is_err());
        assert!(validate_app_name("").is_err());
        assert!(validate_key("short").is_err());
        assert!(validate_key("has space inside").is_err());
        assert!(validate_key("abc&evil=1xx").is_err());
        assert!(validate_key("AbCdEf0123456789_-").is_ok());
    }

    #[test]
    fn response_interpretation_body_text_first() {
        assert_eq!(
            interpret_response(401, br#"{"error":"Invalid API Key"}"#),
            Err(FetchError::BadKey)
        );
        assert_eq!(
            interpret_response(200, br#"{"error":"Invalid API Key"}"#),
            Err(FetchError::BadKey)
        );
        assert_eq!(
            interpret_response(403, b"forbidden"),
            Err(FetchError::BadKey)
        );
        assert_eq!(
            interpret_response(429, b"{}"),
            Err(FetchError::Server { status: 429 })
        );
        assert_eq!(
            interpret_response(200, b"<html>"),
            Err(FetchError::BadResponse)
        );
        let ok = br#"[{"urls":"stun:stun.relay.metered.ca:80"},{"urls":"turn:global.relay.metered.ca:80","username":"<USERNAME>","credential":"<CREDENTIAL>"}]"#;
        assert_eq!(interpret_response(200, ok).unwrap().len(), 2);
    }

    #[test]
    fn debug_is_redacted() {
        let c = ApiCredentials {
            app: "secretapp".into(),
            key: "secretkey123".into(),
        };
        assert!(!format!("{c:?}").contains("secret"));
    }
}
