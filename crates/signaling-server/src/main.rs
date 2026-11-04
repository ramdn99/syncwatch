//! `syncwatch-signaling` binary.
//!
//! Environment:
//!   SYNCWATCH_SIGNALING_BIND   listen address (default 0.0.0.0:8787)
//!   SYNCWATCH_TRUST_PROXY=1    use X-Forwarded-For (only behind a TLS reverse proxy you control)
//!   RUST_LOG                   log filter (default info)

use syncwatch_signaling::{Config, Hub, serve};

#[tokio::main]
async fn main() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
    let bind = std::env::var("SYNCWATCH_SIGNALING_BIND").unwrap_or_else(|_| "0.0.0.0:8787".into());
    let cfg = Config {
        trust_proxy: std::env::var("SYNCWATCH_TRUST_PROXY").as_deref() == Ok("1"),
        ..Config::default()
    };
    let listener = match tokio::net::TcpListener::bind(&bind).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(%bind, error = %e, "cannot bind");
            std::process::exit(1);
        }
    };
    tracing::info!(%bind, "syncwatch-signaling listening (/ws, /healthz)");
    let hub = Hub::new(cfg);
    tokio::select! {
        r = serve(listener, hub) => {
            if let Err(e) = r {
                tracing::error!(error = %e, "server error");
                std::process::exit(1);
            }
        }
        _ = tokio::signal::ctrl_c() => tracing::info!("shutting down"),
    }
}
