//! Checking colleagues' identity key attestations before pinning them (e2ee.md §3.1, security
//! review A1). The identity provider's signing keys are fetched from the configured issuer
//! directly, never through the TeamCraft server.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use teamcraft_crypto::attestation::{AttestationExpectations, verify_attestation};
use teamcraft_crypto::identity::IdentityPublicKey;
use teamcraft_proto::{KeyAttestationMode, UserId};

use super::Client;
use crate::api::ServerApi;
use crate::convert;
use crate::store::LocalStore;

/// How soon keys pinned on first use are checked again after the provider's keys couldn't be fetched.
const PROVIDER_RETRY: Duration = Duration::from_secs(60 * 60);

/// Which identity provider attests identity keys. Configurable per deployment; without it the
/// provider the owners pinned in the signed policy does, and without that pins are trust on first use.
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
  /// No attestation configured, nobody signed the person's account, or none presented and none
  /// required.
  TrustOnFirstUse,
  Refused,
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Uses `source` for the identity provider's signing keys (tests, or a platform HTTP stack).
  pub fn set_jwks_source(&mut self, source: Box<dyn JwksSource>) {
    self.maybe_jwks_source = Some(source);
  }

  /// The provider whose attestations count here, if any: the deployment's settings, otherwise the
  /// provider the owners pinned in the signed policy (never the server's word). Required when either
  /// says so.
  pub(crate) fn attestation_settings(&self) -> Option<AttestationSettings> {
    let maybe_signing = self
      .workspace()
      .ok()
      .and_then(|workspace| workspace.policy.maybe_role_signing);
    let is_required_by_policy = KeyAttestationMode::is_required(maybe_signing.as_deref());
    let mut settings = match &self.settings.maybe_attestation {
      Some(settings) => settings.clone(),
      None => {
        let provider = maybe_signing?.maybe_provider?;
        AttestationSettings {
          issuer: provider.issuer,
          audience: provider.client_id,
          is_required: false,
        }
      },
    };
    settings.is_required |= is_required_by_policy;
    Some(settings)
  }

  /// The provider's signing keys, from the provider, never through the server.
  pub(crate) async fn provider_keys(&self, issuer: &str) -> Option<Vec<u8>> {
    match &self.maybe_jwks_source {
      Some(source) => source.fetch().await,
      None => http_jwks_source(issuer).fetch().await,
    }
  }
}

/// The identity provider's signing keys, fetched once for all the keys one round checks, and only
/// when a token needs them.
pub(crate) struct AttestationCheck {
  settings: AttestationSettings,
  keys: ProviderKeys,
}

enum ProviderKeys {
  NotFetched,
  Fetched(Vec<u8>),
  Unavailable,
}

impl AttestationCheck {
  pub(crate) fn new(settings: AttestationSettings) -> Self {
    Self {
      settings,
      keys: ProviderKeys::NotFetched,
    }
  }

  /// When the next check of a key pinned on first use is due, counted from `now_ms`: after
  /// `recheck_ms`, or after [`PROVIDER_RETRY`] when the provider's keys couldn't be fetched, so an
  /// outage doesn't put off an upgrade for a whole recheck interval.
  pub(crate) fn checked_at_ms(&self, now_ms: i64, recheck_ms: i64) -> i64 {
    if !matches!(self.keys, ProviderKeys::Unavailable) {
      return now_ms;
    }
    let retry_ms = i64::try_from(PROVIDER_RETRY.as_millis()).unwrap_or(i64::MAX);
    now_ms
      .saturating_sub(recheck_ms)
      .saturating_add(retry_ms)
      .min(now_ms)
  }

  /// Decides whether `user`'s identity key may be pinned, from the attestation the server passed
  /// along with it.
  ///
  /// The claim in the token is the person's own word (a profile attribute they may edit), so only the
  /// token's subject says whose account vouches for the key. That counts only where the owners signed
  /// the person's subject (the roster's provider marks, e2ee.md §3.1.3): owners and admins. Everybody
  /// else is pinned on first use, also where attestation is required, which locks nobody out.
  pub(crate) async fn check<A: ServerApi, S: LocalStore>(
    &mut self,
    client: &Client<A, S>,
    user: UserId,
    public_key: &[u8; 32],
    maybe_token: Option<&str>,
  ) -> PinCheck {
    let Some(subject) = signed_subject(client, user) else {
      return PinCheck::TrustOnFirstUse;
    };
    let refused = if self.settings.is_required {
      PinCheck::Refused
    } else {
      PinCheck::TrustOnFirstUse
    };
    let Some(token) = maybe_token else {
      tracing::debug!(user_id = %user, "identity key without an attestation");
      return refused;
    };
    if matches!(self.keys, ProviderKeys::NotFetched) {
      self.keys = match client.provider_keys(&self.settings.issuer).await {
        Some(jwks) => ProviderKeys::Fetched(jwks),
        None => ProviderKeys::Unavailable,
      };
    }
    let ProviderKeys::Fetched(jwks) = &self.keys else {
      tracing::warn!("the identity provider's signing keys are unavailable");
      return refused;
    };
    let expected = AttestationExpectations {
      issuer: &self.settings.issuer,
      audience: &self.settings.audience,
    };
    match verify_attestation(
      token,
      jwks,
      &expected,
      convert::user(user),
      &IdentityPublicKey::from_bytes(*public_key),
    ) {
      Ok(verified) if verified.subject == subject => PinCheck::Attested,
      Ok(_) => {
        tracing::warn!(user_id = %user, "identity key attested by another account than the signed one");
        refused
      },
      Err(error) => {
        tracing::warn!(user_id = %user, %error, "identity key attestation failed");
        refused
      },
    }
  }
}

/// `user`'s account at the provider, as the owners signed it; `None` for somebody they never noted.
fn signed_subject<A: ServerApi, S: LocalStore>(client: &Client<A, S>, user: UserId) -> Option<String> {
  let mut workspace = client.workspace().ok()?;
  workspace.provider_marks.remove(&user)?.maybe_subject
}

/// The signing keys of `issuer`, read from the issuer itself: the discovery document names the JWKS
/// URL, which must be on the issuer's host.
pub(crate) fn http_jwks_source(issuer: &str) -> Box<dyn JwksSource> {
  Box::new(HttpJwksSource {
    http: crate::api::http_client(),
    issuer: issuer.trim_end_matches('/').to_string(),
  })
}

struct HttpJwksSource {
  http: reqwest::Client,
  issuer: String,
}

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
