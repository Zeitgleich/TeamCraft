//! TLS trust of the native clients. rustls needs a crypto provider; the dependency graph of the apps
//! enables two (aws-lc-rs through reqwest, ring through the call stack), so rustls can't pick one by
//! itself and panics when a configuration is built without one. Every configuration here names
//! aws-lc-rs, the provider reqwest uses.
//!
//! Roots: on Android, reqwest's default verifier (rustls-platform-verifier) needs a JNI context and a
//! Kotlin component, so HTTP and the WebSockets use the Mozilla root store (`webpki-roots`), which
//! needs nothing from the app. Elsewhere the WebSockets trust the operating system's roots (as
//! reqwest's verifier does for HTTP), so a company CA installed on the machine works for both.

use std::sync::{Arc, OnceLock};

use rustls::{ClientConfig, RootCertStore};

/// One client configuration for the process.
pub(crate) fn client_config() -> Arc<ClientConfig> {
  static CONFIG: OnceLock<Arc<ClientConfig>> = OnceLock::new();
  CONFIG.get_or_init(build_client_config).clone()
}

fn build_client_config() -> Arc<ClientConfig> {
  let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
  let builder = match ClientConfig::builder_with_provider(provider).with_safe_default_protocol_versions() {
    Ok(builder) => builder,
    // Invariant: aws-lc-rs supports the default protocol versions (TLS 1.2 and 1.3).
    Err(error) => unreachable!("aws-lc-rs rejects the default TLS versions: {error}"),
  };
  Arc::new(builder.with_root_certificates(root_store()).with_no_client_auth())
}

#[cfg(target_os = "android")]
fn root_store() -> RootCertStore {
  let mut roots = RootCertStore::empty();
  roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
  roots
}

/// The system's roots (`SSL_CERT_FILE` / `SSL_CERT_DIR` override them, as for OpenSSL); the Mozilla
/// roots if none can be loaded, so a broken system store doesn't cut the client off.
#[cfg(not(target_os = "android"))]
fn root_store() -> RootCertStore {
  let native = rustls_native_certs::load_native_certs();
  for error in &native.errors {
    tracing::warn!(%error, "loading a system root certificate failed");
  }
  let mut roots = RootCertStore::empty();
  let (added, ignored) = roots.add_parsable_certificates(native.certs);
  if ignored > 0 {
    tracing::debug!(ignored, "system root certificates rustls can't use");
  }
  if added == 0 {
    tracing::warn!("no usable system root certificates, using the Mozilla roots");
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
  }
  roots
}
