//! Signaling client (spec section 2, pitfall 4): errors name the host tried and the reason
//! (DNS, refused, timeout, TLS, HTTP) and never contain credentials.

use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use std::time::Duration;
use syncwatch_protocol::signaling::*;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

/// Compiled-in production URL (placeholder until deployment, spec section 1).
pub const DEFAULT_SIGNALING_URL: &str = "wss://signal.example.invalid/ws";
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
/// Client keepalive interval (the server closes connections idle for 60 s).
pub const KEEPALIVE_EVERY: Duration = Duration::from_secs(20);

/// The URL to use: `SYNCWATCH_SIGNALING_URL` if set, else the compiled-in default.
pub fn configured_url() -> String {
    std::env::var("SYNCWATCH_SIGNALING_URL").unwrap_or_else(|_| DEFAULT_SIGNALING_URL.to_owned())
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SignalingError {
    #[error("the signaling URL is not valid (use wss://host/ws; ws:// only for localhost)")]
    BadUrl,
    #[error("Could not reach the signaling server {host}: the name could not be resolved (DNS)")]
    Dns { host: String },
    #[error("Could not reach the signaling server {host}: connection refused")]
    Refused { host: String },
    #[error("Could not reach the signaling server {host}: timed out")]
    Timeout { host: String },
    #[error("Could not reach the signaling server {host}: secure connection (TLS) failed")]
    Tls { host: String },
    #[error("Could not reach the signaling server {host}: HTTP {status}")]
    Http { host: String, status: u16 },
    #[error("The signaling server {host} uses a different protocol version")]
    Version { host: String },
    #[error("The signaling server {host} sent an unexpected answer")]
    Protocol { host: String },
    #[error("The connection to the signaling server {host} was lost")]
    Closed { host: String },
}

struct Target {
    host: String,
    port: u16,
    tls: bool,
}

fn parse_target(url: &str) -> Result<Target, SignalingError> {
    let (tls, rest) = if let Some(r) = url.strip_prefix("wss://") {
        (true, r)
    } else if let Some(r) = url.strip_prefix("ws://") {
        (false, r)
    } else {
        return Err(SignalingError::BadUrl);
    };
    let authority = rest.split('/').next().unwrap_or_default();
    if authority.is_empty() || authority.contains('@') {
        return Err(SignalingError::BadUrl);
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if !h.ends_with(']') || authority.starts_with('[') => (
            h.trim_matches(['[', ']']).to_owned(),
            p.parse::<u16>().map_err(|_| SignalingError::BadUrl)?,
        ),
        _ => (authority.to_owned(), if tls { 443 } else { 80 }),
    };
    let local = matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1");
    if !tls && !local {
        return Err(SignalingError::BadUrl);
    }
    Ok(Target { host, port, tls })
}

fn tls_config() -> Result<Arc<rustls::ClientConfig>, ()> {
    use rustls_platform_verifier::BuilderVerifierExt;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let cfg = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|_| ())?
        .with_platform_verifier()
        .map_err(|_| ())?
        .with_no_client_auth();
    Ok(Arc::new(cfg))
}

type WsStream = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>;

/// A connected, version-checked signaling session.
pub struct SignalingConn {
    ws: WsStream,
    pub host: String,
    last_sent: std::time::Instant,
}

impl SignalingConn {
    pub async fn connect(url: &str) -> Result<Self, SignalingError> {
        let t = parse_target(url)?;
        let host = t.host.clone();
        let err_host = || host.clone();
        // DNS first, so the error can say exactly what failed.
        let addrs: Vec<std::net::SocketAddr> = tokio::time::timeout(
            CONNECT_TIMEOUT,
            tokio::net::lookup_host((t.host.as_str(), t.port)),
        )
        .await
        .map_err(|_| SignalingError::Timeout { host: err_host() })?
        .map_err(|_| SignalingError::Dns { host: err_host() })?
        .collect();
        let addr = *addrs
            .first()
            .ok_or(SignalingError::Dns { host: err_host() })?;
        let tcp = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr))
            .await
            .map_err(|_| SignalingError::Timeout { host: err_host() })?
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::ConnectionRefused => {
                    SignalingError::Refused { host: err_host() }
                }
                std::io::ErrorKind::TimedOut => SignalingError::Timeout { host: err_host() },
                _ => SignalingError::Refused { host: err_host() },
            })?;
        let _ = tcp.set_nodelay(true);
        let request = url
            .into_client_request()
            .map_err(|_| SignalingError::BadUrl)?;
        let connector = if t.tls {
            Some(tokio_tungstenite::Connector::Rustls(
                tls_config().map_err(|_| SignalingError::Tls { host: err_host() })?,
            ))
        } else {
            Some(tokio_tungstenite::Connector::Plain)
        };
        let (ws, _) = tokio::time::timeout(
            CONNECT_TIMEOUT,
            tokio_tungstenite::client_async_tls_with_config(request, tcp, None, connector),
        )
        .await
        .map_err(|_| SignalingError::Timeout { host: err_host() })?
        .map_err(|e| match e {
            tokio_tungstenite::tungstenite::Error::Http(r) => SignalingError::Http {
                host: err_host(),
                status: r.status().as_u16(),
            },
            tokio_tungstenite::tungstenite::Error::Tls(_) => {
                SignalingError::Tls { host: err_host() }
            }
            tokio_tungstenite::tungstenite::Error::Io(_) if t.tls => {
                SignalingError::Tls { host: err_host() }
            }
            _ => SignalingError::Protocol { host: err_host() },
        })?;
        let mut c = Self {
            ws,
            host,
            last_sent: std::time::Instant::now(),
        };
        c.send(&ClientMsg::Hello {
            v: SIGNALING_VERSION,
        })
        .await?;
        match c.recv().await? {
            ServerMsg::Welcome { .. } => Ok(c),
            ServerMsg::Error {
                code: SigError::Version,
            } => Err(SignalingError::Version { host: c.host }),
            _ => Err(SignalingError::Protocol { host: c.host }),
        }
    }

    pub async fn send(&mut self, m: &ClientMsg) -> Result<(), SignalingError> {
        let text = serde_json::to_string(m).map_err(|_| SignalingError::Protocol {
            host: self.host.clone(),
        })?;
        self.last_sent = std::time::Instant::now();
        self.ws
            .send(Message::Text(text.into()))
            .await
            .map_err(|_| SignalingError::Closed {
                host: self.host.clone(),
            })
    }

    /// Receives the next server message. Sends a `Ping` whenever nothing was sent for
    /// [`KEEPALIVE_EVERY`], so idle connections stay under the server's idle timeout.
    /// Cancel-safe: dropping the future loses no message.
    pub async fn recv(&mut self) -> Result<ServerMsg, SignalingError> {
        loop {
            let wait = KEEPALIVE_EVERY.saturating_sub(self.last_sent.elapsed());
            let Ok(next) = tokio::time::timeout(wait, self.ws.next()).await else {
                self.send(&ClientMsg::Ping).await?;
                continue;
            };
            match next {
                Some(Ok(Message::Text(t))) => {
                    return parse_server(t.as_str()).map_err(|_| SignalingError::Protocol {
                        host: self.host.clone(),
                    });
                }
                Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_))) => continue,
                _ => {
                    return Err(SignalingError::Closed {
                        host: self.host.clone(),
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_rules() {
        assert!(
            parse_target("wss://signal.example.invalid/ws").is_ok_and(|t| t.tls && t.port == 443)
        );
        assert!(parse_target("ws://127.0.0.1:8787/ws").is_ok_and(|t| !t.tls && t.port == 8787));
        assert!(parse_target("ws://localhost:8787/ws").is_ok());
        assert_eq!(
            parse_target("ws://signal.example.invalid/ws").err(),
            Some(SignalingError::BadUrl),
            "plain ws only for localhost"
        );
        assert_eq!(
            parse_target("https://x/ws").err(),
            Some(SignalingError::BadUrl)
        );
        assert_eq!(
            parse_target("wss://user:pw@x/ws").err(),
            Some(SignalingError::BadUrl)
        );
    }

    #[test]
    fn error_messages_name_the_host_and_reason() {
        let e = SignalingError::Dns {
            host: "signal.example.invalid".into(),
        };
        assert_eq!(
            e.to_string(),
            "Could not reach the signaling server signal.example.invalid: the name could not be resolved (DNS)"
        );
        assert!(
            SignalingError::Refused { host: "h".into() }
                .to_string()
                .contains("refused")
        );
        assert!(
            SignalingError::Tls { host: "h".into() }
                .to_string()
                .contains("TLS")
        );
    }
}
