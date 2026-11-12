//! Owners and admins never sign roles by hand (e2ee.md §3.1.3, issue #1276). Two duties, both on the
//! upkeep path and with no request while nothing changed:
//!
//! - **Every app** makes sure the provider's ID token names its identity key and publishes that token
//!   once as the key's attestation (e2ee.md §3.1); **an app of somebody with a provider role** also
//!   publishes it when the roster doesn't say the same yet.
//! - **An owner's app** verifies such tokens against the provider, directly, and signs the next roster,
//!   with nobody asked. The first owner's app signs the first roster the same way, itself alone.
//!
//! The server only carries the tokens. It can hold them back and it can report somebody gone, but
//! nobody gets on the roster without a token the identity provider signed for that person and key.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use teamcraft_crypto::attestation::{
  AttestationError, AttestationExpectations, UnverifiedClaims, attestation_claim, unverified_claims,
  unverified_roles, verify_role_evidence, verify_role_loss,
};
use teamcraft_crypto::identity::IdentityPublicKey;
use teamcraft_proto::{
  Base64Bytes, PutIdentityKeyAttestationRequest, PutRoleEvidenceRequest, RoleProvider, RoleSigningPolicy,
  UserId,
};

use super::Client;
use crate::api::{ApiError, ProviderTokens, ServerApi};
use crate::convert;
use crate::error::CoreError;
use crate::records;
use crate::role_signing::{
  AutomaticBasis, AutomaticChange, AutomaticChangeKind, ProviderMark, ProviderRole, ProviderStatement,
  RoleProviderAnchor, RosterPlanInput, count_provider_move, forget_accounts, plan_roster, provider_role,
  role_loss_statement, unsettled_provider_move,
};
use crate::store::{LocalStore, Table};
use crate::workspace::{RosterDocument, RosterMember, RosterRole, WorkspaceState};

/// Where this device keeps what it did about role signing.
const ROLE_SIGNING_KEY: &str = "role_signing";

/// What this device did about role signing; local state.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleSigningRecord {
  /// The newest token this device published.
  #[serde(rename = "published", default, skip_serializing_if = "Option::is_none")]
  pub maybe_published: Option<PublishedEvidence>,
  /// The one thing a person should know about, if any.
  #[serde(rename = "note", default, skip_serializing_if = "Option::is_none")]
  pub maybe_note: Option<RoleSigningProblem>,
  /// The provider this device got its tokens from before: a server that announces another one can't
  /// make this device take it for the workspace's while no signed policy names one yet.
  #[serde(rename = "seen_provider", default, skip_serializing_if = "Option::is_none")]
  pub maybe_seen_provider: Option<String>,
  /// Since when, by the provider's clock, this device has seen the server report each person gone.
  #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
  pub reports: BTreeMap<UserId, i64>,
  /// When this device last took somebody off on such a report.
  #[serde(
    rename = "last_report_removal",
    default,
    skip_serializing_if = "Option::is_none"
  )]
  pub maybe_last_report_removal: Option<i64>,
  /// The attestation claim of the identity key this device published a token for (e2ee.md §3.1):
  /// another key (after a reset) needs another one.
  #[serde(rename = "attested_key", default, skip_serializing_if = "Option::is_none")]
  pub maybe_attested_key: Option<String>,
}

/// The token this device published last.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishedEvidence {
  pub issued_at: i64,
  pub role: PublishedRole,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublishedRole {
  Owner,
  Admin,
  Neither,
}

/// What keeps role signing from working by itself. Said once, in the console, to the people it
/// concerns, and written to the log for the operator (operations.md).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "problem", rename_all = "snake_case")]
pub enum RoleSigningProblem {
  /// ID tokens carry no roles where the signed policy reads them: the realm needs the mapper, or the
  /// policy the provider's claim (`roles_claim`).
  NoRoleClaim,
  /// The provider didn't store the identity key attribute, or the account API refused it.
  NoKeyClaim,
  /// The provider's tokens come from another issuer or client than the signed policy names.
  ProviderChanged { issuer: String },
  /// Evidence contradicts what is signed, or the server reports an owner gone; somebody has to look.
  Contradictory { users: Vec<UserId> },
  /// The server reports these admins gone. Nothing newer says otherwise, so they go in a while.
  ReportedGone { users: Vec<UserId> },
  /// The provider makes this user an owner, and no owner's app has signed that yet.
  AwaitingOwner,
  #[serde(other)]
  Unknown,
}

/// What a running client remembers between rounds.
#[derive(Debug, Default)]
pub(crate) struct RoleSigningRun {
  /// The ID token the last round looked at.
  maybe_seen_token: Option<String>,
  /// Somebody published newer evidence, or a round that wanted to read it failed.
  is_evidence_dirty: bool,
  /// The gateway connection evidence was read for: a new one may have missed an event.
  maybe_read_connection: Option<u64>,
  /// No further try before this after a failure (device clock: this only paces retries).
  retry_at_ms: i64,
  retries: u32,
  /// The role in the last token: a change is worth a look at the evidence.
  maybe_seen_role: Option<ProviderRole>,
  /// The attribute was written, and the time of the token this device held then: a refresh brings
  /// a token with it, and a later token without it means the realm doesn't map it.
  maybe_written: Option<WrittenAttribute>,
  /// The provider showed it doesn't take or map the attribute: somebody with no role doesn't ask it
  /// again until the app starts anew.
  is_attribute_unavailable: bool,
  /// The attestation this device made sure the server holds since the app started, or the server
  /// refused (e.g. an older server without the endpoint): not asked again until the app starts anew.
  maybe_attestation_settled: Option<String>,
}

#[derive(Debug)]
struct WrittenAttribute {
  claim: String,
  maybe_token_issued_at: Option<i64>,
  /// Whether it was written once more because a later token didn't name it: somebody may have
  /// overwritten it. After that, it is the realm.
  was_rewritten: bool,
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// One round; nothing but a local comparison while nothing changed. Returns whether the store
  /// changed.
  pub async fn run_role_signing(&mut self) -> Result<bool, CoreError> {
    let Some(tokens) = self.api.provider_tokens() else {
      return Ok(false);
    };
    let maybe_connection = self.gateway_connections();
    let is_new_token = self.role_signing.maybe_seen_token.as_deref() != Some(tokens.id_token.as_str());
    let is_new_connection =
      maybe_connection.is_some() && maybe_connection != self.role_signing.maybe_read_connection;
    if !is_new_token && !self.role_signing.is_evidence_dirty && !is_new_connection {
      return Ok(false);
    }
    let now = self.now_ms();
    if now < self.role_signing.retry_at_ms {
      return Ok(false);
    }
    let needs_evidence = self.role_signing.is_evidence_dirty || is_new_connection;
    match self.role_signing_round(&tokens, needs_evidence).await {
      Ok(is_changed) => {
        self.role_signing.maybe_seen_token = Some(tokens.id_token);
        self.role_signing.maybe_read_connection = maybe_connection;
        self.role_signing.is_evidence_dirty = false;
        self.role_signing.retries = 0;
        self.role_signing.retry_at_ms = 0;
        Ok(is_changed)
      },
      Err(error) => {
        self.role_signing.is_evidence_dirty = needs_evidence;
        let backoff = self
          .settings
          .role_signing_retry
          .saturating_mul(2u32.saturating_pow(self.role_signing.retries.min(6)));
        self.role_signing.retries = self.role_signing.retries.saturating_add(1);
        self.role_signing.retry_at_ms =
          now.saturating_add(i64::try_from(backoff.as_millis()).unwrap_or(i64::MAX));
        Err(error)
      },
    }
  }

  /// Somebody published newer evidence: the next round reads it.
  pub fn note_role_evidence_update(&mut self) {
    self.role_signing.is_evidence_dirty = true;
    self.role_signing.retry_at_ms = 0;
  }

  /// What this device knows about role signing.
  pub fn role_signing_record(&self) -> Result<RoleSigningRecord, CoreError> {
    role_signing_record(&self.store)
  }

  /// Everything but publishing and reading evidence is local, so a token refresh costs nothing while
  /// the roster already says what the token says. `needs_evidence`: read what others published.
  async fn role_signing_round(
    &mut self,
    tokens: &ProviderTokens,
    needs_evidence: bool,
  ) -> Result<bool, CoreError> {
    let claims =
      unverified_claims(&tokens.id_token).map_err(|error| CoreError::Corrupt(error.to_string()))?;
    let state = self.workspace()?;
    let maybe_anchor = self
      .anchor_for(&state, &claims)
      .filter(|_| self.is_the_provider_seen_before(&state, &claims).unwrap_or(false));
    let Some(anchor) = maybe_anchor else {
      // Another provider than the owners signed or this device knows: nothing it says or reads counts.
      self.set_problem(Some(RoleSigningProblem::ProviderChanged {
        issuer: claims.issuer.clone(),
      }))?;
      return Ok(true);
    };
    // What this device signed in at before: signing in at the same provider after a reset is no move.
    let maybe_previous_issuer = self.role_signing_record()?.maybe_seen_provider;
    self.remember_provider(&claims)?;
    // Every member's key, role or not: what colleagues' apps check before pinning it (e2ee.md §3.1).
    let expected = self.identity_attestation_claim();
    let has_key_claim = claims.maybe_key_claim.as_deref() == Some(expected.as_str());
    if has_key_claim {
      self.publish_identity_attestation(tokens, &expected).await?;
    }
    // Before anything else, mapper or not: without a pin, a server could later announce another issuer.
    if self.is_verified_owner(&state)
      && state
        .policy
        .maybe_role_signing
        .as_deref()
        .is_none_or(|policy| policy.maybe_provider.is_none())
    {
      self
        .pin_provider(&anchor, maybe_previous_issuer.as_deref())
        .await?;
    }
    let state = self.workspace()?;
    let maybe_roles = unverified_roles(&tokens.id_token, &anchor.client_id, &anchor.roles_claim);
    let role = provider_role(maybe_roles.as_deref(), &anchor);
    let my_entry = state
      .roster
      .members
      .iter()
      .find(|member| member.user_id == self.account.user_id);
    if role == ProviderRole::NotSaid {
      // The usual reason on an instance that updated: the mapper isn't there yet, which the access token
      // shows by still naming the role. Only people it concerns hear of it.
      if is_provider_admin(tokens, &anchor) {
        tracing::warn!(
          roles_claim = %roles_claim_label(&anchor.roles_claim),
          "the identity provider's ID tokens carry no roles in the claim the signed policy reads, so role \
           changes can't be signed by themselves: add the role mapper, or name the claim in the console \
           (docs/operations.md, \"Role signing\")"
        );
        self.set_problem(Some(RoleSigningProblem::NoRoleClaim))?;
        return Ok(true);
      }
      // Without it there, this person has no role left: the mapper adds no claim then. Whatever goes to
      // the server now is refused, and the server keeps the access token as the evidence. An app that sits
      // idle sends nothing, so it knocks once per token, for as long as the roster lists them.
      if my_entry.is_some()
        && let Err(error) = self
          .api
          .put_role_evidence(&PutRoleEvidenceRequest {
            token: tokens.id_token.clone(),
          })
          .await
      {
        tracing::debug!(%error, "the server refused this token, as it should for no role");
      }
      return self
        .attest_as_member(tokens, &claims, &expected, has_key_claim)
        .await;
    }
    let concerns_me = matches!(role, ProviderRole::Owner | ProviderRole::Admin) || my_entry.is_some();
    if !concerns_me {
      return self
        .attest_as_member(tokens, &claims, &expected, has_key_claim)
        .await;
    }
    if !has_key_claim {
      return self.write_identity_attribute(tokens, &claims, &expected).await;
    }
    self.publish_own_evidence(tokens, &claims, &state, role).await?;
    self.clear_transient_problem()?;
    let is_role_news = self.role_signing.maybe_seen_role != Some(role);
    // A move the owners signed waits for an owner's app to sign the roster that follows it.
    let is_move_unsettled =
      unsettled_provider_move(state.policy.maybe_role_signing.as_deref(), state.provider_moves).is_some();
    if !needs_evidence && !is_role_news && !is_move_unsettled {
      return Ok(false);
    }
    let is_changed = self.sign_what_the_provider_says(tokens, &anchor, role).await?;
    self.role_signing.maybe_seen_role = Some(role);
    Ok(is_changed)
  }

  /// `false` if no signed policy names a provider yet and this device got its tokens from another one
  /// before: a server's `server-info` announcing a look-alike issuer shows as just that. An owner's
  /// explicit move to a new provider says so in the signed policy.
  fn is_the_provider_seen_before(
    &self,
    state: &WorkspaceState,
    claims: &UnverifiedClaims,
  ) -> Result<bool, CoreError> {
    let maybe_policy = state.policy.maybe_role_signing.as_deref();
    let is_decided =
      maybe_policy.is_some_and(|policy| policy.maybe_provider.is_some() || policy.accepts_new_provider);
    if is_decided {
      return Ok(true);
    }
    let record = self.role_signing_record()?;
    Ok(
      record
        .maybe_seen_provider
        .as_deref()
        .is_none_or(|seen| seen.trim_end_matches('/') == claims.issuer.trim_end_matches('/')),
    )
  }

  fn remember_provider(&self, claims: &UnverifiedClaims) -> Result<(), CoreError> {
    let mut record = self.role_signing_record()?;
    if record.maybe_seen_provider.as_deref() == Some(claims.issuer.as_str()) {
      return Ok(());
    }
    record.maybe_seen_provider = Some(claims.issuer.clone());
    // A token of the provider before says nothing to the owners' apps now: the next one goes out.
    record.maybe_published = None;
    records::put_json(&self.store, Table::Workspace, ROLE_SIGNING_KEY, &record)
  }

  /// The provider whose tokens count here: the owners' signed word, or, while there is none, the
  /// provider this device itself got its token from. `None` when the token is from another provider
  /// than the signed one.
  fn anchor_for(&self, state: &WorkspaceState, claims: &UnverifiedClaims) -> Option<RoleProviderAnchor> {
    let maybe_policy = state.policy.maybe_role_signing.as_deref();
    match RoleProviderAnchor::signed(maybe_policy) {
      Some(anchor) => {
        let is_same = anchor.issuer.trim_end_matches('/') == claims.issuer.trim_end_matches('/')
          && claims.audience.contains(&anchor.client_id);
        is_same.then_some(anchor)
      },
      None => Some(RoleProviderAnchor::of_own_token(
        &claims.issuer,
        claims
          .maybe_authorized_party
          .as_deref()
          .or_else(|| claims.audience.first().map(String::as_str))
          .unwrap_or_default(),
        maybe_policy,
      )),
    }
  }

  /// Publishes the token as the attestation of this device's identity key, once per key. A failure
  /// fails the round, which retries it with backoff.
  async fn publish_identity_attestation(
    &mut self,
    tokens: &ProviderTokens,
    claim: &str,
  ) -> Result<(), CoreError> {
    if self.role_signing.maybe_attestation_settled.as_deref() == Some(claim) {
      return Ok(());
    }
    let mut record = self.role_signing_record()?;
    // Published before: once per start, look whether the server still holds it (a restored database
    // loses it, and colleagues then take this key on first use).
    if record.maybe_attested_key.as_deref() == Some(claim) && self.is_attestation_on_the_server().await? {
      self.role_signing.maybe_attestation_settled = Some(claim.to_owned());
      return Ok(());
    }
    let request = PutIdentityKeyAttestationRequest {
      token: tokens.id_token.clone(),
    };
    match self.api.put_identity_key_attestation(&request).await {
      Ok(()) => {},
      // A refusal is no reason to stop the round: the owner's duties come after this (an older server
      // doesn't know the endpoint, or the key changed meanwhile). Asked again after a restart.
      Err(error) if is_permanent_refusal(&error) => {
        tracing::warn!(%error, "the server refused this device's key attestation");
        self.role_signing.maybe_attestation_settled = Some(claim.to_owned());
        return Ok(());
      },
      Err(error) => return Err(error.into()),
    }
    self.role_signing.maybe_attestation_settled = Some(claim.to_owned());
    record.maybe_attested_key = Some(claim.to_owned());
    records::put_json(&self.store, Table::Workspace, ROLE_SIGNING_KEY, &record)
  }

  async fn is_attestation_on_the_server(&self) -> Result<bool, CoreError> {
    let maybe_key = self.api.identity_key(self.account.user_id).await?;
    Ok(maybe_key.is_some_and(|key| {
      key.public_key.as_slice() == self.identity_public_key().as_bytes() && key.maybe_attestation.is_some()
    }))
  }

  /// Somebody with no role: their token only attests their identity key, so the attribute is written
  /// if the token doesn't name it yet. Not again on a realm that showed it doesn't take it, where
  /// colleagues pin the key on first use as before.
  async fn attest_as_member(
    &mut self,
    tokens: &ProviderTokens,
    claims: &UnverifiedClaims,
    expected: &str,
    has_key_claim: bool,
  ) -> Result<bool, CoreError> {
    if has_key_claim || self.role_signing.is_attribute_unavailable {
      return Ok(false);
    }
    self.write_identity_attribute(tokens, claims, expected).await
  }

  /// The ID token doesn't name this identity key yet: write the attribute, and the next token will.
  async fn write_identity_attribute(
    &mut self,
    tokens: &ProviderTokens,
    claims: &UnverifiedClaims,
    expected: &str,
  ) -> Result<bool, CoreError> {
    let mut is_rewrite = false;
    if let Some(written) = self
      .role_signing
      .maybe_written
      .as_ref()
      .filter(|written| written.claim == expected)
    {
      let is_later = claims.maybe_issued_at > written.maybe_token_issued_at;
      if is_later && !written.was_rewritten {
        // Somebody may have overwritten it since: once more.
        is_rewrite = true;
      } else if is_later {
        // Written twice, and later tokens still don't name it: the realm has no mapper for it.
        tracing::warn!(
          "the identity provider's ID tokens don't carry the identity key, so role changes can't be \
           signed by themselves (docs/operations.md, \"Role signing\")"
        );
        self.role_signing.is_attribute_unavailable = true;
        self.set_problem(Some(RoleSigningProblem::NoKeyClaim))?;
        return Ok(true);
      } else {
        // The app's next refresh brings it. Nothing to do until then.
        self.api.ask_for_new_id_token();
        return Ok(false);
      }
    }
    let written = crate::attestation::write_identity_attribute(
      &crate::api::http_client(),
      &claims.issuer,
      &tokens.access_token,
      tokens.maybe_prover.as_deref(),
      expected,
    )
    .await;
    match written {
      Ok(()) => {
        self.role_signing.maybe_written = Some(WrittenAttribute {
          claim: expected.to_owned(),
          maybe_token_issued_at: claims.maybe_issued_at,
          was_rewritten: is_rewrite,
        });
        self.api.ask_for_new_id_token();
        Ok(false)
      },
      Err(
        crate::attestation::AttestationError::NotPermitted
        | crate::attestation::AttestationError::AttributeNotStored
        // A provider without such an API.
        | crate::attestation::AttestationError::Status(404 | 405),
      ) => {
        tracing::warn!(
          "the identity provider doesn't take the identity key attribute, so role changes can't be \
           signed by themselves (docs/operations.md, \"Role signing\")"
        );
        self.role_signing.is_attribute_unavailable = true;
        self.set_problem(Some(RoleSigningProblem::NoKeyClaim))?;
        Ok(true)
      },
      Err(error) => Err(CoreError::Api(ApiError::Network(error.to_string()))),
    }
  }

  /// Publishes this device's token if the roster doesn't say what it says yet: a role the roster
  /// doesn't have, or an account nobody has noted. Otherwise nothing goes out.
  async fn publish_own_evidence(
    &mut self,
    tokens: &ProviderTokens,
    claims: &UnverifiedClaims,
    state: &WorkspaceState,
    role: ProviderRole,
  ) -> Result<(), CoreError> {
    let Some(issued_at) = claims.maybe_issued_at else {
      return Ok(());
    };
    let published_role = match role {
      ProviderRole::Owner => PublishedRole::Owner,
      ProviderRole::Admin => PublishedRole::Admin,
      _ => PublishedRole::Neither,
    };
    if !self.needs_publishing(state, published_role) {
      return Ok(());
    }
    let mut record = self.role_signing_record()?;
    let interval_secs = i64::try_from(self.settings.role_evidence_refresh.as_secs()).unwrap_or(i64::MAX);
    // What the owners' apps took already, or set a floor above, doesn't count: they need a newer one.
    let floor = state
      .provider_marks
      .get(&self.account.user_id)
      .map_or(i64::MIN, |mark| mark.issued_at);
    let is_recent = record.maybe_published.as_ref().is_some_and(|published| {
      published.role == published_role
        && published.issued_at > floor
        && issued_at.saturating_sub(published.issued_at) < interval_secs
    });
    if is_recent {
      return Ok(());
    }
    self
      .api
      .put_role_evidence(&PutRoleEvidenceRequest {
        token: tokens.id_token.clone(),
      })
      .await?;
    record.maybe_published = Some(PublishedEvidence {
      issued_at,
      role: published_role,
    });
    records::put_json(&self.store, Table::Workspace, ROLE_SIGNING_KEY, &record)
  }

  fn needs_publishing(&self, state: &WorkspaceState, role: PublishedRole) -> bool {
    let me = self.account.user_id;
    let maybe_member = state.roster.members.iter().find(|member| member.user_id == me);
    let on_roster = maybe_member.map(|member| &member.role);
    let is_said = matches!(
      (role, on_roster),
      (PublishedRole::Owner, Some(RosterRole::Owner))
        | (PublishedRole::Admin, Some(RosterRole::Admin))
        | (PublishedRole::Neither, None)
    );
    let is_my_key = maybe_member
      .is_none_or(|member| member.identity_key.as_slice() == self.identity_public_key().as_bytes());
    let is_noted = role == PublishedRole::Neither
      || state
        .provider_marks
        .get(&me)
        .is_some_and(|mark| mark.maybe_subject.is_some());
    !(is_said && is_my_key && is_noted)
  }

  /// An owner's duty: verifies the evidence and signs the next roster (with the first owner's app, the
  /// first one). Reads the server only when something may have changed.
  async fn sign_what_the_provider_says(
    &mut self,
    tokens: &ProviderTokens,
    anchor: &RoleProviderAnchor,
    role: ProviderRole,
  ) -> Result<bool, CoreError> {
    self.sync_workspace().await?;
    let state = self.workspace()?;
    if !RoleSigningPolicy::is_automatic(state.policy.maybe_role_signing.as_deref()) {
      return Ok(false);
    }
    // An owner of the roster signs also when the provider took the role away: that is how a handover
    // gets through, as nobody else could sign the successor. What is signed still rests on tokens only,
    // and their own departure with it. Everybody else has nothing to sign; a provider owner waits.
    if state.roster.version > 0 && !self.is_verified_owner(&state) {
      if role == ProviderRole::Owner {
        self.set_problem(Some(RoleSigningProblem::AwaitingOwner))?;
      }
      return Ok(false);
    }
    if state.roster.version == 0 && role != ProviderRole::Owner {
      return Ok(false);
    }
    let Some(jwks) = self.provider_keys(&anchor.issuer).await else {
      return Err(CoreError::Api(ApiError::Network(
        "the identity provider's signing keys are unavailable".to_owned(),
      )));
    };
    let own = match verified_statement(
      &tokens.id_token,
      &jwks,
      anchor,
      self.account.user_id,
      &self.identity_public_key_bytes(),
    ) {
      Ok(statement) => statement,
      Err(error) => {
        tracing::warn!(%error, "this device's own ID token doesn't verify; nothing is signed");
        return Ok(false);
      },
    };
    if state.roster.version == 0 {
      if own.role != ProviderRole::Owner {
        return Ok(false);
      }
      return self.sign_first_roster(own, anchor).await;
    }
    self.sign_next_roster(state, own, &jwks, anchor).await
  }

  /// The first roster: this owner alone, as their own provider token proves. Everybody else comes
  /// after, from their own tokens. A server can't steer this: it decides nothing about who is on it,
  /// and a device that pinned a root, or whose owner's other device did, doesn't start a new one.
  async fn sign_first_roster(
    &mut self,
    own: ProviderStatement,
    anchor: &RoleProviderAnchor,
  ) -> Result<bool, CoreError> {
    if self.workspace()?.maybe_root.is_some() || self.account_data().maybe_workspace_root.is_some() {
      return Ok(false);
    }
    let issued_at = own.issued_at;
    tracing::info!("signing the first roster from this owner's provider token");
    let mut document = RosterDocument {
      members: vec![RosterMember {
        user_id: self.account.user_id,
        role: RosterRole::Owner,
        identity_key: own.identity_key.clone(),
      }],
      ..RosterDocument::default()
    };
    document.provider_marks.insert(
      self.account.user_id,
      ProviderMark {
        maybe_subject: Some(own.subject),
        issued_at,
        maybe_by_hand: None,
      },
    );
    let change = AutomaticChange {
      user_id: self.account.user_id,
      change: AutomaticChangeKind::Owner,
      basis: AutomaticBasis::ProviderToken,
      maybe_issued_at: Some(issued_at),
    };
    self.append_roster_automatically(document, vec![change]).await?;
    self.pin_provider(anchor, None).await?;
    Ok(true)
  }

  async fn sign_next_roster(
    &mut self,
    state: WorkspaceState,
    own: ProviderStatement,
    jwks: &[u8],
    anchor: &RoleProviderAnchor,
  ) -> Result<bool, CoreError> {
    let listed = self.api.list_role_evidence().await?;
    // After a move the owners signed, the accounts noted at the provider before name nobody at this one.
    let maybe_move =
      unsettled_provider_move(state.policy.maybe_role_signing.as_deref(), state.provider_moves);
    let marks = match maybe_move {
      Some(_) => forget_accounts(&state.provider_marks),
      None => state.provider_marks.clone(),
    };
    let mut statements = vec![own.clone()];
    for entry in &listed.entries {
      if entry.user_id == self.account.user_id {
        continue;
      }
      let Ok(public_key) = <[u8; 32]>::try_from(entry.public_key.as_slice()) else {
        continue;
      };
      match verified_statement(&entry.token, jwks, anchor, entry.user_id, &public_key) {
        Ok(statement) => statements.push(statement),
        // A hostile server's token, or one from before this person's current key: skipped.
        Err(error) => tracing::debug!(user_id = %entry.user_id, %error, "role evidence refused"),
      }
    }
    statements.extend(role_loss_statements(&listed, &state, jwks, anchor));
    let pinned: BTreeMap<UserId, [u8; 32]> = records::pinned_identities(&self.store)?
      .into_iter()
      .map(|(user, identity)| (user, identity.public_key))
      .collect();
    let secs = |duration: std::time::Duration| i64::try_from(duration.as_secs()).unwrap_or(i64::MAX);
    let mut record = self.role_signing_record()?;
    let plan = plan_roster(RosterPlanInput {
      roster: &state.roster,
      marks: &marks,
      statements: &statements,
      withdrawn: &listed.withdrawn,
      me: self.account.user_id,
      provider_now: own.issued_at,
      max_evidence_age_secs: secs(self.settings.role_evidence_max_age),
      pinned: &pinned,
      reports: &record.reports,
      maybe_last_report_removal: record.maybe_last_report_removal,
      report_grace_secs: secs(self.settings.role_report_grace),
      report_interval_secs: secs(self.settings.role_report_interval),
    });
    record.reports.clone_from(&plan.reports);
    record.maybe_last_report_removal = plan.maybe_last_report_removal;
    records::put_json(&self.store, Table::Workspace, ROLE_SIGNING_KEY, &record)?;
    self.note_what_people_should_know(&plan.contradicted, &plan.reported)?;
    let has_changes = plan.has_changes() || maybe_move.is_some();
    if has_changes {
      if let Some(moves) = maybe_move {
        tracing::info!(
          provider_moves = moves,
          "forgetting the accounts noted at the identity provider before the move"
        );
      }
      for change in &plan.changes {
        tracing::info!(
          user_id = %change.user_id,
          change = ?change.change,
          basis = ?change.basis,
          "changing the roster on the identity provider's word"
        );
      }
      let mut document = state.roster.clone();
      document.members = plan.members;
      document.provider_marks = plan.marks;
      if let Some(moves) = maybe_move {
        document.provider_moves = moves;
      }
      self.append_roster_automatically(document, plan.changes).await?;
    }
    // The owner before signed themselves off and couldn't sign the policy under this roster: until
    // somebody does, they could still sign one that names an older roster (A7).
    let state = self.workspace()?;
    let is_policy_behind = state.policy_version > 0 && state.policy_roster_version < state.roster.version;
    if is_policy_behind && self.is_verified_owner(&state) {
      self.publish_policy(state.policy).await?;
    }
    Ok(has_changes || is_policy_behind)
  }

  /// Writes the provider into the signed policy, so from now on the server's word doesn't count for
  /// it. On an instance that updated this is the moment it is pinned: from the token this device got
  /// itself, not from anything the server says.
  /// `maybe_previous_issuer`: the provider this device signed in at before, if it knows one.
  async fn pin_provider(
    &mut self,
    anchor: &RoleProviderAnchor,
    maybe_previous_issuer: Option<&str>,
  ) -> Result<(), CoreError> {
    let state = self.workspace()?;
    let mut policy = state.policy;
    let mut signing = policy.maybe_role_signing.take().unwrap_or_default();
    // A move the owners signed: the roster's accounts belong to the provider before (e2ee.md §3.1.3).
    count_provider_move(
      &mut signing,
      state.provider_moves,
      maybe_previous_issuer,
      &anchor.issuer,
    );
    signing.maybe_provider = Some(RoleProvider {
      issuer: anchor.issuer.clone(),
      client_id: anchor.client_id.clone(),
    });
    signing.accepts_new_provider = false;
    policy.maybe_role_signing = Some(signing);
    self.publish_policy(policy).await
  }

  /// Says at most one thing: what needs a look first, then who is about to go on a report.
  fn note_what_people_should_know(
    &self,
    contradicted: &[UserId],
    reported: &[UserId],
  ) -> Result<(), CoreError> {
    let maybe_problem = if !contradicted.is_empty() {
      tracing::warn!(
        count = contradicted.len(),
        "role evidence contradicts the signed roster"
      );
      Some(RoleSigningProblem::Contradictory {
        users: contradicted.to_vec(),
      })
    } else if !reported.is_empty() {
      Some(RoleSigningProblem::ReportedGone {
        users: reported.to_vec(),
      })
    } else {
      None
    };
    self.set_problem(maybe_problem)
  }

  fn set_problem(&self, maybe_problem: Option<RoleSigningProblem>) -> Result<(), CoreError> {
    let mut record = self.role_signing_record()?;
    if record.maybe_note == maybe_problem {
      return Ok(());
    }
    record.maybe_note = maybe_problem;
    records::put_json(&self.store, Table::Workspace, ROLE_SIGNING_KEY, &record)
  }

  /// A round got through: what stopped earlier ones doesn't any more.
  fn clear_transient_problem(&self) -> Result<(), CoreError> {
    let record = self.role_signing_record()?;
    if matches!(
      record.maybe_note,
      Some(
        RoleSigningProblem::NoRoleClaim
          | RoleSigningProblem::NoKeyClaim
          | RoleSigningProblem::ProviderChanged { .. }
          | RoleSigningProblem::AwaitingOwner
      )
    ) {
      return self.set_problem(None);
    }
    Ok(())
  }

  /// What the provider's `tc_identity_key` claim has to say about this user and key.
  pub fn identity_attestation_claim(&self) -> String {
    attestation_claim(convert::user(self.account.user_id), &self.identity_public_key())
  }

  /// This user's public identity key.
  pub fn identity_public_key_bytes(&self) -> [u8; 32] {
    *self.identity_public_key().as_bytes()
  }

  fn identity_public_key(&self) -> IdentityPublicKey {
    self.account.identity_public_key()
  }
}

/// The claim role evidence is read from, for the log.
fn roles_claim_label(roles_claim: &[String]) -> String {
  if roles_claim.is_empty() {
    "resource_access.<client>.roles".to_owned()
  } else {
    roles_claim.join(".")
  }
}

/// Whether the access token gives this user an owner or admin role: the people a realm's missing mapper
/// concerns. Only decides who hears of it, so nothing is verified.
fn is_provider_admin(tokens: &ProviderTokens, anchor: &RoleProviderAnchor) -> bool {
  let maybe_roles = unverified_roles(&tokens.access_token, &anchor.client_id, &anchor.roles_claim);
  matches!(
    provider_role(maybe_roles.as_deref(), anchor),
    ProviderRole::Owner | ProviderRole::Admin
  )
}

/// The expired access tokens the server kept, as statements about people on the roster
/// ([`role_loss_statement`]). One that doesn't verify says nothing.
fn role_loss_statements(
  listed: &teamcraft_proto::ListRoleEvidenceResponse,
  state: &WorkspaceState,
  jwks: &[u8],
  anchor: &RoleProviderAnchor,
) -> Vec<ProviderStatement> {
  let expected = AttestationExpectations {
    issuer: &anchor.issuer,
    audience: &anchor.client_id,
  };
  let mut statements = Vec::new();
  for loss in &listed.role_losses {
    let Some(member) = state
      .roster
      .members
      .iter()
      .find(|member| member.user_id == loss.user_id)
    else {
      continue;
    };
    let evidence = match verify_role_loss(&loss.token, jwks, &expected, &anchor.roles_claim) {
      Ok(evidence) => evidence,
      Err(error) => {
        tracing::debug!(user_id = %loss.user_id, %error, "role loss evidence refused");
        continue;
      },
    };
    let role = provider_role(evidence.maybe_roles.as_deref(), anchor);
    statements.extend(role_loss_statement(
      member,
      state.provider_marks.get(&loss.user_id),
      evidence.subject,
      evidence.issued_at,
      role,
    ));
  }
  statements
}

/// What this device knows about role signing, from its store.
pub fn role_signing_record(store: &impl LocalStore) -> Result<RoleSigningRecord, CoreError> {
  Ok(records::get_json(store, Table::Workspace, ROLE_SIGNING_KEY)?.unwrap_or_default())
}

/// `token` read as a statement about `user`, if every check passes.
fn verified_statement(
  token: &str,
  jwks: &[u8],
  anchor: &RoleProviderAnchor,
  user: UserId,
  public_key: &[u8; 32],
) -> Result<ProviderStatement, AttestationError> {
  let expected = AttestationExpectations {
    issuer: &anchor.issuer,
    audience: &anchor.client_id,
  };
  let evidence = verify_role_evidence(
    token,
    jwks,
    &expected,
    &anchor.roles_claim,
    convert::user(user),
    &IdentityPublicKey::from_bytes(*public_key),
  )?;
  Ok(ProviderStatement {
    user_id: user,
    identity_key: Base64Bytes::new(public_key.to_vec()),
    subject: evidence.subject,
    issued_at: evidence.issued_at,
    role: provider_role(evidence.maybe_roles.as_deref(), anchor),
  })
}

/// A `4xx` the same request would get again; `408` and `429` pass with time, so they fail the round
/// and are retried like a network error.
fn is_permanent_refusal(error: &ApiError) -> bool {
  matches!(error, ApiError::Status { status, .. } if (400..500).contains(status) && !matches!(status, 408 | 429))
}
