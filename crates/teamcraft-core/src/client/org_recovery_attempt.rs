//! A new device asks the organization to recover its account (e2ee.md §6.3.1): the person lost
//! every device and the recovery kit, and the IT department decides.
//!
//! One implementation for every client: the phones (teamcraft-ffi), the desktop core and the
//! browser's in-page core all start, watch and finish a request through [`OrgRecoveryAttempt`].
//! The one-time request key lives only in memory: a restart loses it, and the device starts a new
//! request (with a new code), cancelling the old one.

use std::future::Future;
use std::sync::Arc;

use rand_core::OsRng;
#[cfg(feature = "ui")]
use teamcraft_client_api::remote::{OrgRecoveryStage, OrgRecoveryStatus};
use teamcraft_crypto::org_recovery::{RecoveryRequest, RecoveryRequestKey};
use teamcraft_proto::{
  Base64Bytes, CreateRecoveryRequest, RecoveryRequestId, RecoveryRequestResponse, RecoveryRequestStatus,
  RecoveryResultPayload, UserId,
};

use super::enroll::{EnrollmentSecret, NewBackupSecret, OrgRecoveryUnlock};
use super::org_recovery::recovery_request;
use crate::api::ApiError;
use crate::error::CoreError;

/// A recovery request this device made and still waits on.
pub struct OrgRecoveryAttempt {
  /// Shared with the enrollment that finishes the request, so a failed finish can be retried.
  key: Arc<RecoveryRequestKey>,
  request_id: RecoveryRequestId,
  user: UserId,
}

/// How [`OrgRecoveryAttempt::start`] went.
pub enum OrgRecoveryStart {
  Started(OrgRecoveryAttempt),
  /// The organization didn't set recovery up, it is switched off for this person, or there is
  /// nothing to recover: the IT department can only reset the account.
  Unavailable,
}

/// Where a request stands, as the recovering device shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrgRecoveryProgress {
  /// Waiting for admins to approve.
  Waiting {
    approvals: u32,
    required_approvals: u32,
  },
  /// Enough admins approved; the last approver's device unlocks the key for this device.
  Approved,
  /// The key arrived: the device can finish.
  Ready,
  Denied,
  Expired,
  /// Withdrawn (on another device, or by a newer request of this one).
  Cancelled,
}

/// The server endpoints of a recovery request, as the recovering device uses them.
pub trait OrgRecoveryRequestApi {
  /// `POST /org-recovery/requests`
  fn create_recovery_request(
    &self,
    request: &CreateRecoveryRequest,
  ) -> impl Future<Output = Result<RecoveryRequestResponse, ApiError>>;
  /// `GET /org-recovery/requests/{id}`
  fn recovery_request(
    &self,
    id: RecoveryRequestId,
  ) -> impl Future<Output = Result<RecoveryRequestResponse, ApiError>>;
  /// `GET /org-recovery/requests/{id}/result`: `409` until the combining admin handed it in.
  fn recovery_result(
    &self,
    id: RecoveryRequestId,
  ) -> impl Future<Output = Result<RecoveryResultPayload, ApiError>>;
  /// `DELETE /org-recovery/requests/{id}`
  fn cancel_recovery_request(&self, id: RecoveryRequestId) -> impl Future<Output = Result<(), ApiError>>;
}

impl OrgRecoveryAttempt {
  /// Asks the organization for recovery with a fresh one-time key. A refusal because recovery
  /// isn't there for this person (`403`, `409`) is [`OrgRecoveryStart::Unavailable`], not an error.
  pub async fn start(api: &impl OrgRecoveryRequestApi) -> Result<OrgRecoveryStart, CoreError> {
    let key = RecoveryRequestKey::generate(&mut OsRng);
    let body = CreateRecoveryRequest {
      request_public_key: Base64Bytes::new(key.public_key().as_bytes().to_vec()),
    };
    match api.create_recovery_request(&body).await {
      Ok(reply) => Ok(OrgRecoveryStart::Started(Self {
        key: Arc::new(key),
        request_id: reply.id,
        user: reply.user.id,
      })),
      Err(error) if error.is_forbidden() || error.is_conflict() => Ok(OrgRecoveryStart::Unavailable),
      Err(error) => Err(error.into()),
    }
  }

  pub fn request_id(&self) -> RecoveryRequestId {
    self.request_id
  }

  /// The code the person reads to the admin, who compares it before approving.
  pub fn verification_code(&self) -> String {
    self.request().verification_code()
  }

  /// Asks the server where the request stands.
  pub async fn progress(&self, api: &impl OrgRecoveryRequestApi) -> Result<OrgRecoveryProgress, CoreError> {
    let reply = api.recovery_request(self.request_id).await?;
    Ok(progress_of(&reply))
  }

  /// Once the admins answered: what enrolls this device, protecting the account with
  /// `new_secret` from then on (the admins saw the old backup key). `None` while the key is not
  /// there yet. The attempt stays, so a failed enrollment can be tried again.
  pub async fn enrollment_secret(
    &self,
    api: &impl OrgRecoveryRequestApi,
    new_secret: NewBackupSecret,
  ) -> Result<Option<EnrollmentSecret>, CoreError> {
    let result = match api.recovery_result(self.request_id).await {
      Ok(result) => result,
      Err(error) if error.is_conflict() => return Ok(None),
      Err(error) => return Err(error.into()),
    };
    Ok(Some(EnrollmentSecret::OrgRecovery {
      unlock: OrgRecoveryUnlock {
        key: Arc::clone(&self.key),
        request: self.request(),
        sealed_backup_key: result.sealed_backup_key.into_vec(),
      },
      new_secret,
    }))
  }

  /// Withdraws the request, so no admin approves one whose key is gone.
  pub async fn cancel(api: &impl OrgRecoveryRequestApi, id: RecoveryRequestId) -> Result<(), CoreError> {
    match api.cancel_recovery_request(id).await {
      // Already closed (approved and done, denied, expired): nothing to withdraw.
      Err(error) if error.is_conflict() || error.is_not_found() => Ok(()),
      other => Ok(other?),
    }
  }

  fn request(&self) -> RecoveryRequest {
    recovery_request(self.request_id, self.user, self.key.public_key())
  }
}

/// The web and desktop setup screens' organization recovery (teamcraft-client-api `remote`): the
/// one request the in-page core or the desktop core holds while the person goes through the steps.
#[cfg(feature = "ui")]
#[derive(Debug, Default)]
pub struct OrgRecoveryFlow {
  maybe_attempt: Option<OrgRecoveryAttempt>,
}

#[cfg(feature = "ui")]
impl OrgRecoveryFlow {
  /// Starts a new request. The one this core held, and `maybe_previous` (a request a page made
  /// before it reloaded, whose key is gone), are withdrawn first.
  pub async fn start(
    &mut self,
    api: &impl OrgRecoveryRequestApi,
    maybe_previous: Option<RecoveryRequestId>,
  ) -> Result<OrgRecoveryStatus, CoreError> {
    let held = self.maybe_attempt.take().map(|attempt| attempt.request_id());
    for id in held
      .into_iter()
      .chain(maybe_previous.filter(|id| Some(*id) != held))
    {
      // Best effort: an open request whose key is gone can only expire.
      if let Err(error) = OrgRecoveryAttempt::cancel(api, id).await {
        tracing::info!(%error, "could not withdraw an earlier recovery request");
      }
    }
    match OrgRecoveryAttempt::start(api).await? {
      OrgRecoveryStart::Started(attempt) => {
        let status = attempt.status(OrgRecoveryStage::Sent);
        self.maybe_attempt = Some(attempt);
        Ok(status)
      },
      OrgRecoveryStart::Unavailable => Ok(OrgRecoveryStatus::Unavailable),
    }
  }

  /// Where the open request stands; `NotStarted` without one.
  pub async fn status(&self, api: &impl OrgRecoveryRequestApi) -> Result<OrgRecoveryStatus, CoreError> {
    let Some(attempt) = &self.maybe_attempt else {
      return Ok(OrgRecoveryStatus::NotStarted);
    };
    let progress = attempt.progress(api).await?;
    Ok(attempt.status(progress.into()))
  }

  /// The person went back: withdraws the open request.
  pub async fn cancel(&mut self, api: &impl OrgRecoveryRequestApi) -> Result<(), CoreError> {
    match self.maybe_attempt.take() {
      Some(attempt) => OrgRecoveryAttempt::cancel(api, attempt.request_id()).await,
      None => Ok(()),
    }
  }

  /// What enrolls this device once the admins approved, protected by the new kit's key from then
  /// on. Refused without an open request or before the key arrived.
  pub async fn enrollment_secret(
    &self,
    api: &impl OrgRecoveryRequestApi,
    recovery_key: &str,
  ) -> Result<EnrollmentSecret, CoreError> {
    let attempt = self
      .maybe_attempt
      .as_ref()
      .ok_or_else(|| CoreError::InvalidRequest("no recovery request is open on this device".into()))?;
    let new_secret = NewBackupSecret::RecoveryKey(super::enroll::parse_recovery_key(recovery_key)?);
    attempt
      .enrollment_secret(api, new_secret)
      .await?
      .ok_or_else(|| CoreError::InvalidRequest("the admins have not approved the recovery yet".into()))
  }

  /// The device is set up: the request is done.
  pub fn finished(&mut self) {
    self.maybe_attempt = None;
  }
}

#[cfg(feature = "ui")]
impl OrgRecoveryAttempt {
  fn status(&self, stage: OrgRecoveryStage) -> OrgRecoveryStatus {
    OrgRecoveryStatus::Requested {
      request_id: self.request_id,
      code: self.verification_code(),
      stage,
    }
  }
}

#[cfg(feature = "ui")]
impl From<OrgRecoveryProgress> for OrgRecoveryStage {
  fn from(progress: OrgRecoveryProgress) -> Self {
    match progress {
      OrgRecoveryProgress::Waiting {
        approvals,
        required_approvals,
      } => Self::Waiting {
        approvals,
        required_approvals,
      },
      OrgRecoveryProgress::Approved => Self::Approved,
      OrgRecoveryProgress::Ready => Self::Ready,
      OrgRecoveryProgress::Denied => Self::Denied,
      OrgRecoveryProgress::Expired => Self::Expired,
      OrgRecoveryProgress::Cancelled => Self::Cancelled,
    }
  }
}

impl std::fmt::Debug for OrgRecoveryAttempt {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("OrgRecoveryAttempt")
      .field("request_id", &self.request_id)
      .finish_non_exhaustive()
  }
}

/// What the server's view of a request means for the recovering device.
fn progress_of(reply: &RecoveryRequestResponse) -> OrgRecoveryProgress {
  match &reply.status {
    _ if reply.has_result => OrgRecoveryProgress::Ready,
    RecoveryRequestStatus::Approved | RecoveryRequestStatus::Completed => OrgRecoveryProgress::Approved,
    RecoveryRequestStatus::Denied => OrgRecoveryProgress::Denied,
    RecoveryRequestStatus::Expired => OrgRecoveryProgress::Expired,
    RecoveryRequestStatus::Cancelled => OrgRecoveryProgress::Cancelled,
    // A status this client doesn't know yet: still waiting, as far as it can tell.
    RecoveryRequestStatus::Pending | RecoveryRequestStatus::Unknown(_) => OrgRecoveryProgress::Waiting {
      approvals: u32::try_from(reply.approvals.len()).unwrap_or(u32::MAX),
      required_approvals: reply.required_approvals,
    },
  }
}

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {
  use std::cell::RefCell;
  use std::collections::VecDeque;

  use teamcraft_crypto::recovery_key::RecoveryKey;

  use super::*;

  const REQUEST: &str = "77";
  const USER: &str = "5";

  mod start {
    use super::*;

    #[tokio::test]
    async fn a_new_request_shows_the_code_of_its_key() {
      let api = FakeRecoveryApi::answering_start(Ok(request_json("pending", 0, false)));
      let OrgRecoveryStart::Started(attempt) = OrgRecoveryAttempt::start(&api).await.unwrap() else {
        panic!("expected a started request");
      };
      assert_eq!(attempt.request_id().to_string(), REQUEST);
      let sent = (*api.sent_key.borrow()).unwrap();
      let expected = teamcraft_crypto::org_recovery::RecoveryRequestPublicKey::from_bytes(sent).fingerprint();
      assert_eq!(attempt.verification_code(), expected);
    }

    #[tokio::test]
    async fn switched_off_or_not_set_up_is_unavailable() {
      for status in [403, 409] {
        let api = FakeRecoveryApi::answering_start(Err(status));
        assert!(matches!(
          OrgRecoveryAttempt::start(&api).await,
          Ok(OrgRecoveryStart::Unavailable)
        ));
      }
    }

    #[tokio::test]
    async fn another_refusal_is_an_error() {
      let api = FakeRecoveryApi::answering_start(Err(500));
      assert!(OrgRecoveryAttempt::start(&api).await.is_err());
    }
  }

  mod progress {
    use super::*;

    #[test]
    fn counts_the_approvals_while_pending() {
      assert_eq!(
        progress_of(&request_json("pending", 1, false)),
        OrgRecoveryProgress::Waiting {
          approvals: 1,
          required_approvals: 2
        }
      );
    }

    #[test]
    fn a_result_makes_it_ready() {
      assert_eq!(
        progress_of(&request_json("approved", 2, true)),
        OrgRecoveryProgress::Ready
      );
      assert_eq!(
        progress_of(&request_json("approved", 2, false)),
        OrgRecoveryProgress::Approved
      );
    }

    #[test]
    fn closed_requests_say_why() {
      assert_eq!(
        progress_of(&request_json("denied", 0, false)),
        OrgRecoveryProgress::Denied
      );
      assert_eq!(
        progress_of(&request_json("expired", 0, false)),
        OrgRecoveryProgress::Expired
      );
      assert_eq!(
        progress_of(&request_json("cancelled", 0, false)),
        OrgRecoveryProgress::Cancelled
      );
    }

    #[test]
    fn an_unknown_status_still_waits() {
      assert!(matches!(
        progress_of(&request_json("on_hold", 0, false)),
        OrgRecoveryProgress::Waiting { .. }
      ));
    }
  }

  mod finish {
    use super::*;

    #[tokio::test]
    async fn waits_while_the_result_is_not_there() {
      let api = FakeRecoveryApi::answering_start(Ok(request_json("pending", 0, false)));
      let attempt = started(&api).await;
      api.results.borrow_mut().push_back(Err(409));
      assert!(
        attempt
          .enrollment_secret(&api, new_secret())
          .await
          .unwrap()
          .is_none()
      );
    }

    #[tokio::test]
    async fn the_attempt_stays_for_a_second_try() {
      let api = FakeRecoveryApi::answering_start(Ok(request_json("pending", 0, false)));
      let attempt = started(&api).await;
      for _ in 0..2 {
        api.results.borrow_mut().push_back(Ok(vec![1, 2, 3]));
        let Some(EnrollmentSecret::OrgRecovery { unlock, .. }) =
          attempt.enrollment_secret(&api, new_secret()).await.unwrap()
        else {
          panic!("expected an organization recovery secret");
        };
        assert_eq!(unlock.sealed_backup_key, vec![1, 2, 3]);
        assert_eq!(unlock.request.verification_code(), attempt.verification_code());
      }
    }

    #[tokio::test]
    async fn a_network_failure_is_an_error() {
      let api = FakeRecoveryApi::answering_start(Ok(request_json("pending", 0, false)));
      let attempt = started(&api).await;
      api.results.borrow_mut().push_back(Err(503));
      assert!(attempt.enrollment_secret(&api, new_secret()).await.is_err());
    }
  }

  #[tokio::test]
  async fn cancelling_a_closed_request_is_fine() {
    for status in [404, 409] {
      let api = FakeRecoveryApi::answering_start(Err(500));
      *api.cancel_answer.borrow_mut() = Some(status);
      let id: RecoveryRequestId = REQUEST.parse().unwrap();
      OrgRecoveryAttempt::cancel(&api, id).await.unwrap();
    }
    let api = FakeRecoveryApi::answering_start(Err(500));
    *api.cancel_answer.borrow_mut() = Some(503);
    assert!(
      OrgRecoveryAttempt::cancel(&api, REQUEST.parse().unwrap())
        .await
        .is_err()
    );
  }

  #[cfg(feature = "ui")]
  mod flow {
    use super::*;

    #[tokio::test]
    async fn a_new_request_withdraws_the_earlier_ones() {
      let api = FakeRecoveryApi::answering_start(Ok(request_json("pending", 0, false)));
      let mut flow = OrgRecoveryFlow::default();
      let before_reload: RecoveryRequestId = "12".parse().unwrap();
      let status = flow.start(&api, Some(before_reload)).await.unwrap();
      assert!(matches!(
        status,
        OrgRecoveryStatus::Requested {
          stage: OrgRecoveryStage::Sent,
          ..
        }
      ));
      assert_eq!(api.cancelled.borrow().as_slice(), &[before_reload]);
      *api.start.borrow_mut() = Some(Ok(request_json("pending", 0, false)));
      flow.start(&api, None).await.unwrap();
      assert_eq!(api.cancelled.borrow().len(), 2);
      assert_eq!(api.cancelled.borrow()[1].to_string(), REQUEST);
    }

    #[tokio::test]
    async fn without_a_request_there_is_nothing_to_show_or_finish() {
      let api = FakeRecoveryApi::answering_start(Err(500));
      let flow = OrgRecoveryFlow::default();
      assert_eq!(flow.status(&api).await.unwrap(), OrgRecoveryStatus::NotStarted);
      let key = RecoveryKey::generate(&mut OsRng).to_display().to_string();
      assert!(flow.enrollment_secret(&api, &key).await.is_err());
    }

    #[tokio::test]
    async fn finishing_before_the_approval_is_refused() {
      let api = FakeRecoveryApi::answering_start(Ok(request_json("pending", 0, false)));
      let mut flow = OrgRecoveryFlow::default();
      flow.start(&api, None).await.unwrap();
      api.results.borrow_mut().push_back(Err(409));
      let key = RecoveryKey::generate(&mut OsRng).to_display().to_string();
      assert!(matches!(
        flow.enrollment_secret(&api, &key).await,
        Err(CoreError::InvalidRequest(_))
      ));
      assert!(matches!(
        flow.status(&api).await.unwrap(),
        OrgRecoveryStatus::Requested {
          stage: OrgRecoveryStage::Waiting { .. },
          ..
        }
      ));
    }

    #[tokio::test]
    async fn going_back_withdraws_the_request() {
      let api = FakeRecoveryApi::answering_start(Ok(request_json("pending", 0, false)));
      let mut flow = OrgRecoveryFlow::default();
      flow.start(&api, None).await.unwrap();
      flow.cancel(&api).await.unwrap();
      assert_eq!(api.cancelled.borrow().len(), 1);
      assert_eq!(flow.status(&api).await.unwrap(), OrgRecoveryStatus::NotStarted);
    }
  }

  /// Answers the recovering device's calls from queued replies; statuses stand for refusals.
  struct FakeRecoveryApi {
    start: RefCell<Option<Result<RecoveryRequestResponse, u16>>>,
    sent_key: RefCell<Option<[u8; 32]>>,
    results: RefCell<VecDeque<Result<Vec<u8>, u16>>>,
    cancel_answer: RefCell<Option<u16>>,
    cancelled: RefCell<Vec<RecoveryRequestId>>,
  }

  impl FakeRecoveryApi {
    fn answering_start(answer: Result<RecoveryRequestResponse, u16>) -> Self {
      Self {
        start: RefCell::new(Some(answer)),
        sent_key: RefCell::new(None),
        results: RefCell::new(VecDeque::new()),
        cancel_answer: RefCell::new(None),
        cancelled: RefCell::new(Vec::new()),
      }
    }
  }

  impl OrgRecoveryRequestApi for FakeRecoveryApi {
    async fn create_recovery_request(
      &self,
      request: &CreateRecoveryRequest,
    ) -> Result<RecoveryRequestResponse, ApiError> {
      *self.sent_key.borrow_mut() = Some(request.request_public_key.as_slice().try_into().unwrap());
      self.start.borrow_mut().take().unwrap().map_err(refusal)
    }

    async fn recovery_request(&self, _id: RecoveryRequestId) -> Result<RecoveryRequestResponse, ApiError> {
      Ok(request_json("pending", 0, false))
    }

    async fn recovery_result(&self, _id: RecoveryRequestId) -> Result<RecoveryResultPayload, ApiError> {
      let answer = self.results.borrow_mut().pop_front().unwrap();
      answer
        .map(|sealed| RecoveryResultPayload {
          sealed_backup_key: Base64Bytes::new(sealed),
        })
        .map_err(refusal)
    }

    async fn cancel_recovery_request(&self, id: RecoveryRequestId) -> Result<(), ApiError> {
      self.cancelled.borrow_mut().push(id);
      self
        .cancel_answer
        .borrow_mut()
        .take()
        .map_or(Ok(()), |status| Err(refusal(status)))
    }
  }

  async fn started(api: &FakeRecoveryApi) -> OrgRecoveryAttempt {
    match OrgRecoveryAttempt::start(api).await.unwrap() {
      OrgRecoveryStart::Started(attempt) => attempt,
      OrgRecoveryStart::Unavailable => panic!("expected a started request"),
    }
  }

  fn new_secret() -> NewBackupSecret {
    NewBackupSecret::RecoveryKey(RecoveryKey::generate(&mut OsRng))
  }

  fn refusal(status: u16) -> ApiError {
    if status == 503 || status == 500 {
      return ApiError::Network(format!("status {status}"));
    }
    ApiError::Status {
      status,
      maybe_body: None,
      maybe_retry_after: None,
    }
  }

  fn request_json(status: &str, approvals: usize, has_result: bool) -> RecoveryRequestResponse {
    let approvals: Vec<_> = (0..approvals)
      .map(|n| serde_json::json!({ "admin_id": (n + 1).to_string(), "device_id": (n + 10).to_string(), "approved_at": 1 }))
      .collect();
    serde_json::from_value(serde_json::json!({
      "id": REQUEST,
      "user": { "id": USER, "display_name": "Alice", "kind": "member" },
      "fingerprint": "",
      "request_public_key": "",
      "status": status,
      "created_at": 1,
      "expires_at": 2,
      "required_approvals": 2,
      "approvals": approvals,
      "shares_received": 0,
      "has_result": has_result,
    }))
    .unwrap()
  }
}
