//! Organization call backgrounds (admin.md §3.5, e2ee.md §8.1): pictures admins upload, stored
//! unencrypted on the server and listed, with their SHA-256, in the owner-signed workspace policy.
//! A picture is only used after its hash matches the verified policy, so the server can't swap
//! one; owners change the list and the rule by signing a new policy version.

use teamcraft_crypto::attachment::{ciphertext_matches, content_sha256};
use teamcraft_proto::{
  Base64Bytes, CallBackgroundMode, CallBackgroundPolicy, CreateOrgBackgroundUploadRequest, ErrorCode,
  OrgBackground, OrgBackgroundId,
};

use super::Client;
use crate::api::ServerApi;
use crate::error::CoreError;
use crate::store::LocalStore;
use crate::workspace::RosterRole;

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// The organization's backgrounds as the verified workspace policy lists them.
  pub fn org_background_policy(&self) -> Result<CallBackgroundPolicy, CoreError> {
    Ok(self.workspace_policy()?.call_backgrounds)
  }

  /// Downloads an organization background and checks it against the hash in the verified policy.
  pub async fn org_background_picture(&self, id: OrgBackgroundId) -> Result<Vec<u8>, CoreError> {
    let policy = self.org_background_policy()?;
    let listed = policy
      .images
      .iter()
      .find(|image| image.id == id)
      .ok_or_else(|| CoreError::Corrupt("the workspace policy doesn't list this background".into()))?;
    let download = self.api.download_org_background(id).await?;
    let picture = self.api.get_blob(&download.download_url).await?;
    if !ciphertext_matches(&picture, listed.sha256.as_slice()) {
      return Err(CoreError::Corrupt(
        "the organization background doesn't match the signed policy".into(),
      ));
    }
    Ok(picture)
  }

  /// Owners: uploads a picture (a normalized JPEG) and adds it to the signed policy, last in the
  /// order.
  pub async fn add_org_background(
    &mut self,
    name: String,
    picture: Vec<u8>,
  ) -> Result<OrgBackgroundId, CoreError> {
    let created = self
      .api
      .create_org_background_upload(&CreateOrgBackgroundUploadRequest {
        size_bytes: picture.len() as u64,
      })
      .await?;
    let image = OrgBackground {
      id: created.id,
      name,
      sha256: Base64Bytes::new(content_sha256(&picture).to_vec()),
      size_bytes: picture.len() as u64,
    };
    self.api.put_blob(&created.upload_url, picture).await?;
    self.api.complete_org_background_upload(created.id).await?;
    self
      .change_org_backgrounds(|backgrounds| backgrounds.images.push(image))
      .await?;
    Ok(created.id)
  }

  /// Owners: removes a picture from the signed policy (and as default), then deletes it.
  pub async fn delete_org_background(&mut self, id: OrgBackgroundId) -> Result<(), CoreError> {
    self
      .change_org_backgrounds(|backgrounds| {
        backgrounds.images.retain(|image| image.id != id);
        if backgrounds.maybe_default == Some(id) {
          backgrounds.maybe_default = None;
        }
        if backgrounds.mode == CallBackgroundMode::Required && backgrounds.images.is_empty() {
          // Nothing left to require: fall back to the least restrictive rule.
          backgrounds.mode = CallBackgroundMode::Free;
        }
      })
      .await?;
    match self.api.delete_org_background(id).await {
      Err(error) if error.error_code() == Some(&ErrorCode::BackgroundNotFound) => Ok(()),
      other => Ok(other?),
    }
  }

  /// Owners: the rule, default and order. `order` must name every listed picture once.
  pub async fn set_org_background_rule(
    &mut self,
    mode: CallBackgroundMode,
    maybe_default: Option<OrgBackgroundId>,
    guests_exempt: bool,
    order: Vec<OrgBackgroundId>,
  ) -> Result<(), CoreError> {
    let current = self.org_background_policy()?;
    let mut ordered = Vec::with_capacity(order.len());
    for id in &order {
      let image = current
        .images
        .iter()
        .find(|image| image.id == *id)
        .ok_or_else(|| CoreError::InvalidRequest("the order names an unknown background".into()))?;
      ordered.push(image.clone());
    }
    if ordered.len() != current.images.len() {
      return Err(CoreError::InvalidRequest(
        "the order must name every background once".into(),
      ));
    }
    self
      .change_org_backgrounds(|backgrounds| {
        backgrounds.mode = mode;
        backgrounds.maybe_default = maybe_default;
        backgrounds.guests_exempt = guests_exempt;
        backgrounds.images = ordered;
      })
      .await
  }

  /// Signs a new policy version with the backgrounds changed by `change`. A workspace without a
  /// signed roster gets one first, with this user as owner (the root of trust, e2ee.md §11).
  async fn change_org_backgrounds(
    &mut self,
    change: impl FnOnce(&mut CallBackgroundPolicy),
  ) -> Result<(), CoreError> {
    self.sync_workspace().await?;
    if self.workspace()?.roster.version == 0 {
      let me = self.account.user_id;
      self.publish_roster(&[(me, RosterRole::Owner)]).await?;
    }
    let mut policy = self.workspace_policy()?;
    change(&mut policy.call_backgrounds);
    self.publish_policy(policy).await
  }
}
