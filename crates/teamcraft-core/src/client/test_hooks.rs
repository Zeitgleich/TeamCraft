//! Hooks for this crate's own test suite (feature `test-hooks`, only ever a dev-dependency
//! feature, never in an app): a client that plays a modified one.

use teamcraft_crypto::membership::MembershipAction;
use teamcraft_crypto::mls::{MlsError, test_hooks};
use teamcraft_crypto::strict_roles::{SignedStrictRolesChange, StrictRolesChange};
use teamcraft_proto::ConversationId;

use super::Client;
use super::groups::ChangeSource;
use crate::api::ServerApi;
use crate::error::CoreError;
use crate::store::LocalStore;

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Signs `action` with this device and commits it without judging whether this user may make
  /// it, as a modified client could (e2ee.md §4.6.2, finding 3): the server takes a commit from
  /// any member, and the members judge it. Nothing else of the change's flow runs (no server
  /// membership step, no announcement). This device merges its own commit once the server took it,
  /// so it follows what it committed.
  pub async fn commit_unchecked_change(
    &mut self,
    conversation: ConversationId,
    action: MembershipAction,
  ) -> Result<(), CoreError> {
    test_hooks::set_unchecked_commits(true);
    let result = self.commit_change(conversation, ChangeSource::Own(action)).await;
    test_hooks::set_unchecked_commits(false);
    result
  }

  /// Signs a strict channel's roles change with this device as it is, judged by nobody and
  /// committed nowhere, as a modified client could (e2ee.md §4.6.2): what a hostile server puts
  /// into the chain it shows a device that fell out.
  pub fn sign_strict_roles_unchecked(
    &self,
    change: StrictRolesChange,
  ) -> Result<SignedStrictRolesChange, CoreError> {
    change
      .sign(&self.account.device, &self.account.certificate)
      .map_err(|error| MlsError::from(error).into())
  }
}
