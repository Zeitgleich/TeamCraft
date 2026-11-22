//! Guests of a meeting in the phone apps (#1905), as desktop and web have them (`teamcraft-ui`'s
//! "Invite guests", docs/meetings.md "Links" and "Lobby"): a personal one-time link for somebody
//! without an account, a new meeting link that ends the old one, the organizer's code guests may
//! compare, and the lobbies of the meetings the user hosts on the meeting list.
//!
//! Every link handed out carries the organizer's key fingerprint as this device has it pinned
//! (`v=`, e2ee.md §4.6), never the server's: without it no link is made.

use std::collections::HashSet;

use teamcraft_client_api::backend::ChatBackend as _;
use teamcraft_client_api::meeting_links::{organizer_code, with_verifier};
use teamcraft_client_api::meeting_rows::is_joinable;
use teamcraft_client_api::remote::ChatAction;
use teamcraft_core::clock::{Clock as _, SystemClock};
use teamcraft_proto::{CreateGuestLinkRequest, GuestLinkResponse, MeetingId, MeetingResponse, UserId};

use crate::TeamCraft;
use crate::bridge::{from_json, to_json};
use crate::engine::{Engine, parse_id};
use crate::error::TeamCraftError;
use crate::meeting_lobby::{LobbyRead, MeetingLobby, is_lobby_host, lobby_person, signed_membership};

/// A personal one-time link for one guest. It is a credential: the app shows it to the host to
/// copy or share, and never logs it.
#[derive(Clone, PartialEq, Eq, uniffi::Record)]
pub struct MeetingGuestLink {
  pub link: String,
  /// When the link stops working if nobody used it (Unix milliseconds).
  pub expires_at_ms: i64,
}

// The link is a credential and doesn't belong in logs.
impl std::fmt::Debug for MeetingGuestLink {
  fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    formatter
      .debug_struct("MeetingGuestLink")
      .field("link", &"[redacted]")
      .field("expires_at_ms", &self.expires_at_ms)
      .finish()
  }
}

/// The meetings screen's read: the list and the lobbies on it.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct MeetingsWithLobbies {
  /// `GET /meetings` as the server sent it (through the organizer's upkeep).
  pub meetings_json: String,
  /// The lobbies of the meetings the user hosts that can be joined now, where somebody waits.
  pub lobbies: Vec<MeetingLobby>,
}

impl Engine {
  /// A one-time link for a guest of the meeting `meeting_id`, which the user hosts (the server
  /// refuses anybody else). The organizer's key is pinned first, so no link is made on the server
  /// that this device couldn't hand out.
  pub(crate) async fn meeting_guest_link(
    &mut self,
    meeting_id: &str,
  ) -> Result<MeetingGuestLink, TeamCraftError> {
    let meeting = self.meeting(meeting_id).await?;
    let fragment = self.verified_fragment(&meeting).await?;
    let request = CreateGuestLinkRequest { maybe_label: None };
    let made: GuestLinkResponse = from_json(
      &self
        .api_request(
          "POST",
          &format!("/meetings/{}/guest-links", meeting.id),
          Some(to_json(&request)?),
        )
        .await?,
    )?;
    Ok(MeetingGuestLink {
      link: with_verifier(&made.join_url, &fragment),
      expires_at_ms: made.expires_at.0,
    })
  }

  /// Makes the link of the meeting `meeting_id` anew (its organizer only): the old one opens
  /// nothing any more, and the invitees are sent the new one by the server.
  pub(crate) async fn renew_meeting_link(&self, meeting_id: &str) -> Result<(), TeamCraftError> {
    let meeting: MeetingId = parse_id(meeting_id)?;
    self
      .api_request(
        "POST",
        &format!("/meetings/{meeting}/link"),
        Some("{}".to_owned()),
      )
      .await?;
    Ok(())
  }

  /// The organizer's code of the meeting `meeting_id` as this device pins the organizer's key:
  /// what the guest's page shows, for the two to compare.
  pub(crate) async fn meeting_organizer_code(&mut self, meeting_id: &str) -> Result<String, TeamCraftError> {
    let meeting = self.meeting(meeting_id).await?;
    Ok(organizer_code(&self.verified_fragment(&meeting).await?))
  }

  /// The meeting list through the organizer's upkeep ([`Engine::meeting_upkeep`], JSON), and the
  /// lobbies of the meetings on it the user hosts that can be joined now, where somebody waits.
  /// One list request for both; a lobby that can't be read is left out until the next read.
  pub(crate) async fn meetings_with_lobbies(&mut self) -> Result<(String, Vec<LobbyRead>), TeamCraftError> {
    let (json, meetings) = self.meeting_upkeep_list().await?;
    let me = self.backend()?.me().id;
    let mut lobbies = Vec::new();
    for meeting in live_hosted(meetings, me, SystemClock.now_ms()) {
      match self.lobby_of(&meeting).await {
        Ok(lobby) if lobby.is_host && !lobby.entries.is_empty() => lobbies.push(LobbyRead {
          meeting,
          entries: lobby.entries,
        }),
        Ok(_) => {},
        Err(error) => {
          tracing::debug!(meeting_id = %meeting.id, %error, "a meeting's lobby could not be read")
        },
      }
    }
    Ok((json, lobbies))
  }
}

impl TeamCraft {
  /// A host's lobby rows from a read: what the signed roster says of each person, and the key
  /// code of a ready guest (asked of the core where it isn't made yet).
  pub(crate) async fn lobby_of_read(&self, read: LobbyRead) -> Result<MeetingLobby, TeamCraftError> {
    let mut people = Vec::with_capacity(read.entries.len());
    for entry in &read.entries {
      let guest = entry.user.id;
      let membership = self
        .read_queue()?
        .query(move |reads| Ok(signed_membership(reads.backend()?.is_signed_member(guest))))
        .await?;
      let is_ready_guest = matches!(
        teamcraft_core::meeting_lobby::LobbyAdmission::of(entry, membership),
        teamcraft_core::meeting_lobby::LobbyAdmission::Guest { .. }
      );
      let maybe_key_code = if is_ready_guest {
        let maybe_code = self
          .read_queue()?
          .query(move |reads| Ok(reads.backend()?.guest_key_code(guest)))
          .await?;
        if maybe_code.is_none() {
          self
            .act_locally(move |reads| reads.apply_action(ChatAction::PrepareGuestKeyCode { guest }))
            .await?;
        }
        maybe_code
      } else {
        None
      };
      people.push(lobby_person(entry, membership, maybe_key_code));
    }
    Ok(MeetingLobby {
      meeting_id: read.meeting.id.to_string(),
      conversation_id: read.meeting.conversation_id.to_string(),
      people,
    })
  }
}

/// The meetings among `meetings` whose lobby the user hosts and that can be joined at `now`, each
/// once (a series lists every occurrence with its ID).
fn live_hosted(meetings: Vec<MeetingResponse>, me: UserId, now: i64) -> Vec<MeetingResponse> {
  let mut seen = HashSet::new();
  meetings
    .into_iter()
    .filter(|meeting| is_lobby_host(meeting, me) && is_joinable(meeting, now))
    .filter(|meeting| seen.insert(meeting.id))
    .collect()
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{Id, MeetingKind, MeetingStatus, Snowflake, UnixMillis};

  use super::*;

  const NOW: i64 = 1_800_000_000_000;
  const HOUR: i64 = 3_600_000;

  #[test]
  fn lobbies_are_read_for_live_meetings_the_user_hosts_once_each() {
    let running = meeting(7, 1, NOW - HOUR / 2);
    let occurrence_later = meeting(7, 1, NOW + 24 * HOUR);
    let soon = meeting(8, 1, NOW + 10 * 60_000);
    let tomorrow = meeting(9, 1, NOW + 24 * HOUR);
    let somebody_elses = meeting(10, 2, NOW - HOUR / 2);
    let mut cancelled = meeting(11, 1, NOW - HOUR / 2);
    cancelled.status = MeetingStatus::Cancelled;
    let mut co_hosted = meeting(12, 2, NOW);
    co_hosted.co_host_user_ids = vec![id(1)];
    let ids: Vec<MeetingId> = live_hosted(
      vec![
        running,
        occurrence_later,
        soon,
        tomorrow,
        somebody_elses,
        cancelled,
        co_hosted,
      ],
      id(1),
      NOW,
    )
    .iter()
    .map(|meeting| meeting.id)
    .collect();
    assert_eq!(ids, vec![id(7), id(8), id(12)]);
  }

  #[test]
  fn a_guest_link_never_shows_in_logs() {
    let link = MeetingGuestLink {
      link: "https://chat.example.com/meeting/abc#invite=secret&v=11".into(),
      expires_at_ms: NOW,
    };
    assert!(!format!("{link:?}").contains("secret"));
  }

  fn id<T: Id>(raw: i64) -> T {
    T::from_snowflake(Snowflake::new(raw).unwrap())
  }

  /// Meeting `raw_id` of `organizer`, an hour from `starts_at`.
  fn meeting(raw_id: i64, organizer: i64, starts_at: i64) -> MeetingResponse {
    MeetingResponse {
      id: id(raw_id),
      title: "Planning".into(),
      maybe_description: None,
      starts_at: UnixMillis(starts_at),
      ends_at: UnixMillis(starts_at + HOUR),
      kind: MeetingKind::Scheduled,
      status: MeetingStatus::Scheduled,
      has_lobby: true,
      organizer_user_id: id(organizer),
      conversation_id: id(raw_id + 100),
      maybe_space: None,
      join_url: String::new(),
      maybe_recurrence: None,
      is_occurrence_cancelled: false,
      is_occurrence_holiday: false,
      can_host_lobby: false,
      co_host_user_ids: Vec::new(),
      attendees: Vec::new(),
      undelivered_invitations: Vec::new(),
    }
  }
}
