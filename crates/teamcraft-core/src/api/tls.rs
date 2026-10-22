//! TLS trust on Android. reqwest's default verifier there (rustls-platform-verifier) needs a JNI
//! context and a Kotlin component; instead both HTTP and the WebSockets use rustls with the
//! Mozilla root store (`webpki-roots`), which needs nothing from the app.
//!
//! The provider is named explicitly: the tree enables both rustls providers (aws-lc-rs through
//! reqwest, ring through the WebTransport stack), so rustls can't pick a process default and
//! `ClientConfig::builder()` would panic.

use std::sync::{Arc, OnceLock};

use rustls::{ClientConfig, RootCertStore};

/// One client configuration for the process.
pub(crate) fn client_config() -> Arc<ClientConfig> {
  static CONFIG: OnceLock<Arc<ClientConfig>> = OnceLock::new();
  CONFIG
    .get_or_init(|| {
      let mut roots = RootCertStore::empty();
      roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
      let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
      Arc::new(
        ClientConfig::builder_with_provider(provider)
          .with_safe_default_protocol_versions()
          // Invariant: rustls' own provider supports its default protocol versions.
          .expect("aws-lc-rs supports the default TLS versions")
          .with_root_certificates(roots)
          .with_no_client_auth(),
      )
    })
    .clone()
}

/// An HTTP client that trusts the Mozilla roots.
pub(crate) fn http_client() -> reqwest::Client {
  match reqwest::Client::builder()
    .tls_backend_preconfigured((*client_config()).clone())
    .build()
  {
    Ok(client) => client,
    // Only a reqwest/rustls version mismatch can cause this; the default client still works for
    // plain HTTP (development) and reports TLS errors per request.
    Err(error) => {
      tracing::error!(%error, "TLS configuration rejected, using the default HTTP client");
      reqwest::Client::new()
    },
  }
}

