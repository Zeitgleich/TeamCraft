//! TLS trust of the native clients. rustls needs a crypto provider; the dependency graph of the apps
//! enables two (aws-lc-rs through reqwest, ring through the call stack), so rustls can't pick one by
//! itself and panics when a configuration is built without one. Every configuration here names
//! aws-lc-rs, the provider reqwest uses.
//!
//! Roots (issue #157: company CAs and TLS-inspecting proxies):
//! - Desktop and iOS: the platform verifier (rustls-platform-verifier), as reqwest uses for HTTP, so
//!   the WebSockets trust exactly what the operating system trusts: the Keychain with MDM profiles
//!   on macOS and iOS, the Windows certificate store with group policy, the system store on Linux
//!   (`SSL_CERT_FILE` / `SSL_CERT_DIR` override it). If it can't be set up, the Mozilla roots.
//! - Android: the platform verifier would need a JNI context and a Kotlin component, so HTTP and the
//!   WebSockets verify against a root store. The app hands over the device's trusted CAs
//!   ([`set_platform_roots`], from `AndroidCAStore`) before the first connection: the system CAs,
//!   and user or MDM-installed CAs only where the device's management allows it (the app decides,
//!   see docs/operations.md). Without them, the Mozilla roots.

use std::sync::{Arc, PoisonError, RwLock};

use rustls::ClientConfig;

/// Root certificates (DER) the app read from the platform, see the module docs.
static PLATFORM_ROOTS: RwLock<Option<Vec<Vec<u8>>>> = RwLock::new(None);
/// The configuration built from them, until they change.
static CONFIG: RwLock<Option<Arc<ClientConfig>>> = RwLock::new(None);

/// Hands over the platform's trusted root certificates (DER). Connections opened from now on use
/// them; the app reads them off its main thread, so a check of the server address at first start
/// may have used the Mozilla roots already. Used on Android; elsewhere the platform verifier asks
/// the operating system itself.
pub fn set_platform_roots(der_certificates: Vec<Vec<u8>>) {
  *PLATFORM_ROOTS.write().unwrap_or_else(PoisonError::into_inner) = Some(der_certificates);
  *CONFIG.write().unwrap_or_else(PoisonError::into_inner) = None;
}

/// One client configuration for the process (until the platform roots change).
pub(crate) fn client_config() -> Arc<ClientConfig> {
  if let Some(config) = CONFIG.read().unwrap_or_else(PoisonError::into_inner).as_ref() {
    return config.clone();
  }
  let mut slot = CONFIG.write().unwrap_or_else(PoisonError::into_inner);
  slot.get_or_insert_with(build_client_config).clone()
}

fn build_client_config() -> Arc<ClientConfig> {
  let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
  let builder =
    match ClientConfig::builder_with_provider(provider.clone()).with_safe_default_protocol_versions() {
      Ok(builder) => builder,
      // Invariant: aws-lc-rs supports the default protocol versions (TLS 1.2 and 1.3).
      Err(error) => unreachable!("aws-lc-rs rejects the default TLS versions: {error}"),
    };
  #[cfg(not(target_os = "android"))]
  match rustls_platform_verifier::Verifier::new(provider) {
    Ok(verifier) => {
      return Arc::new(
        builder
          .dangerous()
          .with_custom_certificate_verifier(Arc::new(verifier))
          .with_no_client_auth(),
      );
    },
    Err(error) => tracing::warn!(%error, "no platform certificate verifier, using the Mozilla roots"),
  }
  Arc::new(builder.with_root_certificates(root_store()).with_no_client_auth())
}

/// The roots the app handed over, or the Mozilla roots if there are none.
fn root_store() -> rustls::RootCertStore {
  let mut roots = rustls::RootCertStore::empty();
  let platform_roots = PLATFORM_ROOTS.read().unwrap_or_else(PoisonError::into_inner);
  if let Some(platform) = platform_roots.as_ref() {
    let certificates = platform
      .iter()
      .map(|der| rustls::pki_types::CertificateDer::from(der.clone()));
    let (added, ignored) = roots.add_parsable_certificates(certificates);
    if ignored > 0 {
      tracing::debug!(ignored, "platform root certificates rustls can't use");
    }
    if added > 0 {
      return roots;
    }
    tracing::warn!("no usable platform root certificates, using the Mozilla roots");
  }
  roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
  roots
}

/// Whether a connection error is a refused server certificate (rustls'
/// `InvalidCertificate`) somewhere in its chain: the server isn't who it claims, or, much more
/// often, a proxy intercepts TLS with a CA this device doesn't trust. rustls errors travel inside
/// `io::Error`s, nested ones too (hyper wraps the TLS stream's), and `io::Error::source()` skips
/// the error it wraps, so those are followed through `get_ref()`.
pub(crate) fn is_certificate_rejection(error: &(dyn std::error::Error + 'static)) -> bool {
  let mut maybe_error = Some(error);
  while let Some(error) = maybe_error {
    if matches!(
      error.downcast_ref::<rustls::Error>(),
      Some(rustls::Error::InvalidCertificate(_))
    ) {
      return true;
    }
    maybe_error = match error.downcast_ref::<std::io::Error>() {
      Some(io) => match io.get_ref() {
        Some(inner) => Some(inner as &(dyn std::error::Error + 'static)),
        None => std::error::Error::source(io),
      },
      None => error.source(),
    };
  }
  false
}

#[cfg(test)]
mod tests {
  use rustls::CertificateError;

  use super::*;

  #[test]
  fn a_refused_certificate_is_found_inside_io_and_wrapper_errors() {
    let rejected = rustls::Error::InvalidCertificate(CertificateError::UnknownIssuer);
    let io = std::io::Error::new(std::io::ErrorKind::InvalidData, rejected.clone());
    assert!(is_certificate_rejection(&rejected));
    assert!(is_certificate_rejection(&io));
    assert!(is_certificate_rejection(&Wrapper(io)));
    // hyper wraps the TLS stream's `io::Error` in another one.
    let inner = std::io::Error::new(std::io::ErrorKind::InvalidData, rejected);
    let nested = std::io::Error::other(inner);
    assert!(is_certificate_rejection(&Wrapper(nested)));
  }

  #[test]
  fn other_tls_and_io_errors_are_not_certificate_rejections() {
    let alert = rustls::Error::AlertReceived(rustls::AlertDescription::HandshakeFailure);
    assert!(!is_certificate_rejection(&alert));
    let refused = std::io::Error::from(std::io::ErrorKind::ConnectionRefused);
    assert!(!is_certificate_rejection(&Wrapper(refused)));
  }

  /// An error that names its cause only through `source()`, as reqwest's and hyper's do.
  #[derive(Debug)]
  struct Wrapper(std::io::Error);

  impl std::fmt::Display for Wrapper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
      f.write_str("request failed")
    }
  }

  impl std::error::Error for Wrapper {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
      Some(&self.0)
    }
  }
}
