//! SyncWatch connection setup (spec section 5): Metered credential fetch through a strict
//! allowlist, lenient ICE-servers parsing, validation, a standalone STUN/TURN connection test
//! and secure storage. Nothing here ever logs or prints a credential value.

pub mod metered;
pub mod model;
pub mod parse;
pub mod probe;
pub mod store;
pub mod stun;

pub use metered::{ApiCredentials, FetchError, fetch_ice_servers};
pub use model::{IceServer, IceUrl, Scheme, Transport};
pub use parse::{ParseError, parse_ice_servers};
pub use probe::{ProbeError, ProbeKind, TestReport, UrlResult, test_servers};

/// Whether a call installed the provider or found it already in place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderInstall {
    Installed,
    AlreadyInstalled,
}

static PROVIDER: std::sync::Once = std::sync::Once::new();

/// THE one place that installs the process-wide rustls crypto provider (ring), ADR 0004/0009.
/// Called once at app startup (before the torrent engine or any HTTPS client exists). Every
/// later call is a no-op: a second provider can never be installed (a test greps the sources
/// for any other `install_default` call).
pub fn install_crypto_provider() -> ProviderInstall {
    let mut first = false;
    PROVIDER.call_once(|| {
        first = true;
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
    if first {
        ProviderInstall::Installed
    } else {
        ProviderInstall::AlreadyInstalled
    }
}

/// Whether the process default is the ring provider (diagnostics and tests).
pub fn provider_is_ring() -> bool {
    let ring = rustls::crypto::ring::default_provider();
    rustls::crypto::CryptoProvider::get_default().is_some_and(|p| {
        p.cipher_suites.len() == ring.cipher_suites.len()
            && p.cipher_suites
                .iter()
                .zip(ring.cipher_suites.iter())
                .all(|(a, b)| a.suite() == b.suite())
            && p.kx_groups.len() == ring.kx_groups.len()
    })
}
