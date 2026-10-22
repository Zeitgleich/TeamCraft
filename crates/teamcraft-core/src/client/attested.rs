//! Checking colleagues' identity key attestations before pinning them (e2ee.md §3.1, security
//! review A1). The identity provider's signing keys are fetched from the configured issuer
//! directly, never through the TeamCraft server.

use std::future::Future;
use std::pin::Pin;

use teamcraft_crypto::attestation::{AttestationExpectations, verify_attestation};
use teamcraft_crypto::identity::IdentityPublicKey;
use teamcraft_proto::UserId;

use super::Client;
use crate::api::ServerApi;
use crate::convert;
use crate::store::LocalStore;

/// Which identity provider attests identity keys. Configurable per deployment; without it pins
/// are trust on first use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttestationSettings {
  /// The issuer, e.g. `https://id.example.com/realms/company`.
  pub issuer: String,
  /// The TeamCraft client ID ID tokens are issued to.
  pub audience: String,
  /// Refuse to pin identity keys without a valid attestation (instead of trusting on first use).
  pub is_required: bool,
}

/// Fetches the identity provider's JWKS document.
pub trait JwksSource {
  fn fetch(&self) -> Pin<Box<dyn Future<Output = Option<Vec<u8>>> + '_>>;
}

/// How a newly pinned identity key was checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PinCheck {
  Attested,
  /// No attestation configured, or none presented and none required.
  TrustOnFirstUse,
  Refused,
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Uses `source` for the identity provider's signing keys (tests, or a platform HTTP stack).
  pub fn set_jwks_source(&mut self, source: Box<dyn JwksSource>) {
    self.maybe_jwks_source = Some(source);
  }

  /// Decides whether `user`'s identity key may be pinned, from the attestation the server passed
  /// along with it.
  pub(crate) async fn check_attestation(
    &self,
    user: UserId,
    public_key: &[u8; 32],
    maybe_token: Option<&str>,
  ) -> PinCheck {
    let Some(settings) = &self.settings.maybe_attestation else {
      return PinCheck::TrustOnFirstUse;
    };
    let refused = || {
      if settings.is_required {
        PinCheck::Refused
      } else {
        PinCheck::TrustOnFirstUse
      }
    };
    let (Some(token), Some(source)) = (maybe_token, self.maybe_jwks_source.as_ref()) else {
      tracing::warn!(user_id = %user, "identity key without a checkable attestation");
      return refused();
    };
    let Some(jwks) = source.fetch().await else {
      tracing::warn!("the identity provider's signing keys are unavailable");
      return refused();
    };
    let expected = AttestationExpectations {
      issuer: &settings.issuer,
      audience: &settings.audience,
    };
    match verify_attestation(
      token,
      &jwks,
      &expected,
      convert::user(user),
      &IdentityPublicKey::from_bytes(*public_key),
    ) {
      Ok(_) => PinCheck::Attested,
      Err(error) => {
        tracing::warn!(user_id = %user, %error, "identity key attestation failed");
        refused()
      },
    }
  }
}

/// The source a client starts with: the issuer's discovery document names the JWKS URL, which must
/// be on the issuer's host.
pub(crate) fn default_jwks_source(
  maybe_settings: Option<&AttestationSettings>,
) -> Option<Box<dyn JwksSource>> {
  #[cfg(not(target_arch = "wasm32"))]
  {
    maybe_settings.map(|settings| {
      Box::new(HttpJwksSource {
        http: reqwest::Client::new(),
        issuer: settings.issuer.trim_end_matches('/').to_string(),
      }) as Box<dyn JwksSource>
    })
  }
  #[cfg(target_arch = "wasm32")]
  {
    let _ = maybe_settings;
    None
  }
}

#[cfg(not(target_arch = "wasm32"))]
struct HttpJwksSource {
  http: reqwest::Client,
  issuer: String,
}

#[cfg(not(target_arch = "wasm32"))]
impl JwksSource for HttpJwksSource {
  fn fetch(&self) -> Pin<Box<dyn Future<Output = Option<Vec<u8>>> + '_>> {
    Box::pin(async move {
      let discovery: serde_json::Value = self
        .http
        .get(format!("{}/.well-known/openid-configuration", self.issuer))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
      let jwks_uri = url::Url::parse(discovery.get("jwks_uri")?.as_str()?).ok()?;
      let issuer = url::Url::parse(&self.issuer).ok()?;
      if jwks_uri.host_str() != issuer.host_str() || jwks_uri.scheme() != issuer.scheme() {
        return None;
      }
      let body = self.http.get(jwks_uri).send().await.ok()?.bytes().await.ok()?;
      Some(body.to_vec())
    })
  }
}
