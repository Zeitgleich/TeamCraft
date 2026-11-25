//! Server requests beside the engine (#2401): a job on the engine's queue that waits on the
//! network holds up everything behind it (`start`, `stop`, actions, other requests), for as long as
//! a bad network takes, so the apps' plain server requests (the meeting list and lobbies,
//! `api_request`) run on the HTTP runtime ([`run_request`]) and the engine is taken only to apply
//! what came back. Same rule as the read markers (#2355): no engine job awaits the network.
//!
//! The meeting list is read by the meeting watch, the meetings screen and the upkeep, often at the
//! same moment: overlapping reads share one request ([`Coalesced`]). The background reads have a
//! bound on their time on top of the API's per-phase timeouts (connect, answer, idle body): a run
//! whose network went away ends, and the next change or foreground tries again.

use std::time::Duration;

use teamcraft_core::clock::{Clock as _, SystemClock};
use teamcraft_proto::{ListLobbyResponse, MeetingId};

use crate::TeamCraft;
use crate::bridge::from_json;
use crate::coalesced::Coalesced;
use crate::error::TeamCraftError;
use crate::http_fetch::run_request;
use crate::meeting_lobby::lobby_path;
use crate::meetings::meeting_list_path;

/// How long a background read of the meetings may take in all: they are small, and a run that
/// doesn't get them is tried again on the next change or foreground.
const MEETING_READ_TIMEOUT: Duration = Duration::from_secs(20);

/// The meeting list reads that overlapping callers share.
pub(crate) type MeetingLists = Coalesced<Result<String, TeamCraftError>>;

impl TeamCraft {
  /// The user's meetings (`GET /meetings` from an hour ago, JSON as the server sends it), read
  /// beside the engine; callers that overlap share a read.
  pub(crate) async fn meeting_list_json(&self) -> Result<String, TeamCraftError> {
    self
      .meeting_lists
      .run(|| async {
        let path = meeting_list_path(SystemClock.now_ms());
        self.read_beside_engine(path).await
      })
      .await
  }

  /// The lobby of `meeting`, read beside the engine.
  pub(crate) async fn lobby_beside_engine(
    &self,
    meeting: MeetingId,
  ) -> Result<ListLobbyResponse, TeamCraftError> {
    from_json(&self.read_beside_engine(lobby_path(meeting)).await?)
  }

  /// An authenticated request of `/api/v1{path}` on the HTTP runtime: no engine job, so it neither
  /// waits behind the engine's queue nor holds it up. Bounded by the API's per-phase timeouts.
  pub(crate) async fn request_beside_engine(
    &self,
    method: String,
    path: String,
    maybe_body: Option<String>,
  ) -> Result<String, TeamCraftError> {
    self.request_within(method, path, maybe_body, None).await
  }

  /// A background `GET` of a small answer, bounded by [`MEETING_READ_TIMEOUT`] in all.
  async fn read_beside_engine(&self, path: String) -> Result<String, TeamCraftError> {
    self
      .request_within("GET".into(), path, None, Some(MEETING_READ_TIMEOUT))
      .await
  }

  async fn request_within(
    &self,
    method: String,
    path: String,
    maybe_body: Option<String>,
    maybe_bound: Option<Duration>,
  ) -> Result<String, TeamCraftError> {
    let api = self.server_api()?;
    run_request(async move {
      let request = api.raw_request(&method, &path, maybe_body);
      let answer = match maybe_bound {
        Some(bound) => tokio::time::timeout(bound, request)
          .await
          .map_err(|_| TeamCraftError::Network(format!("no answer within {} s", bound.as_secs())))?,
        None => request.await,
      };
      answer.map_err(|error| TeamCraftError::from(teamcraft_core::CoreError::from(error)))
    })
    .await
    .map_err(TeamCraftError::Network)?
  }
}
