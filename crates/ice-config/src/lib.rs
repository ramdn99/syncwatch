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

/// Installs the process-wide rustls crypto provider (ring). Call once at startup.
pub fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}
