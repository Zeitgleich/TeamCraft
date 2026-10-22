//! TLS trust on Android. reqwest's default verifier there (rustls-platform-verifier) needs a JNI
//! context and a Kotlin component; instead both HTTP and the WebSockets use rustls with the
//! Mozilla root store (`webpki-roots`), which needs nothing from the app.

use std::sync::{Arc, OnceLock};

use rustls::{ClientConfig, RootCertStore};

/// One client configuration for the process.
pub(crate) fn client_config() -> Arc<ClientConfig> {
  static CONFIG: OnceLock<Arc<ClientConfig>> = OnceLock::new();
  CONFIG
    .get_or_init(|| {
      let mut roots = RootCertStore::empty();
      roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
      Arc::new(
        ClientConfig::builder()
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
