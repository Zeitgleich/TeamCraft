//! The meeting watch of the phone apps (#2258), as desktop runs it beside its meeting reminders
//! (`teamcraft-ui`'s `watch_meetings`): while the app runs, the user's meeting list is read once
//! per foreground and again whenever the meetings change ([`Engine::meeting_watch`]). Each read
//! keeps the organizer's upkeep current (admitters, owed edits, the hosts and admitters of a
//! meeting just handed to the user), and lets in, by itself, whoever knocked with the meeting link
//! where this device may (docs/meetings.md, "Lobby", step 9; e2ee.md §4.6, "Meeting guests"):
//!
//! - the organizer's device, in a meeting the organizer opened without a lobby on this device
//!   (`is_open_by_own_record`; the server's `has_lobby` counts for nothing);
//! - a member's device in the call of somebody else's meeting, when the organizer's signed
//!   admitters name this user with "no lobby" (`admits_link_arrivals`, read again on each run).
//!
//! Who qualifies is the core's one rule (`teamcraft_core::meeting_lobby::admits_by_itself`, which
//! desktop uses too): only somebody who came with the meeting link, with a device whose key they
//! reported, and a member only by the signed roster. The admitting itself is a host's "Admit"
//! ([`Engine::admit_from_lobby`]). The app says each admission with a "Remove" action
//! ([`crate::TeamCraft::remove_link_arrival`]).

use std::collections::HashSet;

use teamcraft_client_api::backend::ChatBackend as _;
use teamcraft_client_api::meeting_hosts::{MeetingRole, hosting_news};
use teamcraft_client_api::meeting_rows::is_joinable;
use teamcraft_core::client::{admits_link_arrivals, is_open_by_own_record};
use teamcraft_core::clock::{Clock as _, SystemClock};
use teamcraft_core::meeting_lobby::admits_by_itself;
use teamcraft_core::ui_backend::CoreCommand;
use teamcraft_proto::{ConversationId, MeetingId, MeetingResponse, UserId};

use crate::engine::{Engine, parse_id};
use crate::error::TeamCraftError;
use crate::meeting_lobby::signed_membership;

/// Somebody this device let into a meeting by itself: the app says so, with "Remove".
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct LinkArrival {
  pub meeting_id: String,
  pub meeting_title: String,
  /// The meeting's chat: "Remove" takes the person out of it.
  pub conversation_id: String,
  pub user_id: String,
  pub display_name: String,
}

/// What the meeting watch keeps between runs.
#[derive(Default)]
pub(crate) struct MeetingWatchState {
  /// The list the last upkeep read, to tell a meeting newly handed to the user.
  maybe_last_listed: Option<Vec<MeetingResponse>>,
  /// The admissions by itself this device started, one per person and meeting: somebody whose
  /// admission went through no longer waits, and one that failed is taken out to be tried again.
  started: HashSet<(MeetingId, UserId)>,
}

impl MeetingWatchState {
  /// The meetings of `listed` handed to `me` since the last list (#1720); none on the first list,
  /// where nothing is news. Keeps `listed` for the next one.
  pub(crate) fn handed_over(&mut self, listed: &[MeetingResponse], me: UserId) -> Vec<MeetingResponse> {
    let maybe_last = self.maybe_last_listed.replace(listed.to_vec());
    let Some(last) = maybe_last.filter(|last| !last.is_empty()) else {
      return Vec::new();
    };
    hosting_news(&last, listed, me)
      .into_iter()
      .filter(|news| news.role == MeetingRole::Organizer)
      .filter_map(|news| listed.iter().find(|meeting| meeting.id == news.meeting).cloned())
      .collect()
  }
}

impl Engine {
  /// One run of the meeting watch (see the module): the organizer's upkeep on a fresh list, then
  /// the lobbies of the meetings this device may let people of the link into. Returns who was let
  /// in by this run. A lobby that can't be read or an admission that fails is left for the next
  /// run (the next knock or meeting change, or the next foreground).
  pub(crate) async fn meeting_watch(&mut self) -> Result<Vec<LinkArrival>, TeamCraftError> {
    let (_, meetings) = self.meeting_upkeep_list().await?;
    let me = self.backend()?.me().id;
    let now = SystemClock.now_ms();
    let candidates = admitting_candidates(meetings, me, self.calls().active_conversation(), now);
    let live: HashSet<MeetingId> = candidates.iter().map(|meeting| meeting.id).collect();
    self
      .meeting_watch
      .started
      .retain(|(meeting, _)| live.contains(meeting));
    let mut arrivals = Vec::new();
    for meeting in candidates {
      if !self.lets_link_arrivals_in(&meeting, me, now)? {
        continue;
      }
      arrivals.extend(self.admit_link_arrivals_of(&meeting).await?);
    }
    Ok(arrivals)
  }

  /// "Remove" on an admission by itself: takes `user_id` out of the meeting chat
  /// `conversation_id` (out of its encryption group too), as desktop's notice does.
  pub(crate) async fn remove_link_arrival(
    &mut self,
    conversation_id: &str,
    user_id: &str,
  ) -> Result<(), TeamCraftError> {
    let conversation: ConversationId = parse_id(conversation_id)?;
    let user: UserId = parse_id(user_id)?;
    self
      .run_commands(vec![CoreCommand::RemoveMember { conversation, user }])
      .await
  }

  /// Tells the server this app lets guests of the meeting link in by itself (#987), so guests of
  /// the user's meetings aren't told the organizer's app needs an update.
  pub(crate) async fn mark_admitter(&self) -> Result<(), TeamCraftError> {
    self
      .api_request("PUT", "/meetings/admitter", Some("{}".to_owned()))
      .await
      .map(drop)
  }

  /// Whether this device lets people of the link into `meeting` by itself now: the organizer's own
  /// record on the organizer's device; on anybody else's, the organizer's signed admitters, read
  /// again (queued) on each run so a change counts from the next run on. The core tells the app
  /// (a meetings change) once a newly verified list opens the door, which runs the watch again.
  fn lets_link_arrivals_in(
    &mut self,
    meeting: &MeetingResponse,
    me: UserId,
    now: i64,
  ) -> Result<bool, TeamCraftError> {
    if meeting.organizer_user_id == me {
      return Ok(is_open_by_own_record(&self.store, meeting.id));
    }
    let chat = meeting.conversation_id;
    self.backend()?.check_meeting_admission(chat);
    Ok(admits_link_arrivals(&self.store, chat, now))
  }

  /// Lets in whoever waits in the lobby of `meeting` and qualifies (the core's
  /// `admits_by_itself`), each once.
  async fn admit_link_arrivals_of(
    &mut self,
    meeting: &MeetingResponse,
  ) -> Result<Vec<LinkArrival>, TeamCraftError> {
    let lobby = match self.lobby_of(meeting).await {
      Ok(lobby) => lobby,
      Err(error) => {
        tracing::debug!(meeting_id = %meeting.id, %error, "a meeting's lobby could not be read");
        return Ok(Vec::new());
      },
    };
    let mut arrivals = Vec::new();
    for entry in lobby.entries {
      let user = entry.user.id;
      let membership = signed_membership(self.backend()?.is_signed_member(user));
      if !admits_by_itself(true, &entry, membership) || !self.meeting_watch.started.insert((meeting.id, user))
      {
        continue;
      }
      let display_name = entry.user.display_name.clone();
      match self.admit_from_lobby(meeting, entry).await {
        Ok(()) => arrivals.push(LinkArrival {
          meeting_id: meeting.id.to_string(),
          meeting_title: meeting.title.clone(),
          conversation_id: meeting.conversation_id.to_string(),
          user_id: user.to_string(),
          display_name,
        }),
        Err(error) => {
          tracing::warn!(meeting_id = %meeting.id, %error, "could not let in somebody who came with the meeting link");
          self.meeting_watch.started.remove(&(meeting.id, user));
        },
      }
    }
    Ok(arrivals)
  }
}

/// The meetings among `meetings` whose lobby this device may let people of the link into, each
/// once (a series lists every occurrence with its ID): the ones `me` organizes that can be joined
/// at `now`, and the one whose call this device is in (`maybe_call`) if somebody else organizes
/// it. Whether the door is open is decided per meeting afterwards.
fn admitting_candidates(
  meetings: Vec<MeetingResponse>,
  me: UserId,
  maybe_call: Option<ConversationId>,
  now: i64,
) -> Vec<MeetingResponse> {
  let mut seen = HashSet::new();
  meetings
    .into_iter()
    .filter(|meeting| {
      meeting.status == teamcraft_proto::MeetingStatus::Scheduled && is_joinable(meeting, now)
    })
    .filter(|meeting| meeting.organizer_user_id == me || Some(meeting.conversation_id) == maybe_call)
    .filter(|meeting| seen.insert(meeting.id))
    .collect()
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{Id, MeetingKind, MeetingStatus, Snowflake, UnixMillis};

  use super::*;

  const NOW: i64 = 1_800_000_000_000;
  const HOUR: i64 = 3_600_000;

  mod candidates {
    use super::*;

    #[test]
    fn the_organizers_live_meetings_are_watched_once_each() {
      let running = meeting(7, 1, NOW - HOUR / 2);
      let later_occurrence = meeting(7, 1, NOW + 24 * HOUR);
      let soon = meeting(8, 1, NOW + 60_000);
      let tomorrow = meeting(9, 1, NOW + 24 * HOUR);
      let mut cancelled = meeting(10, 1, NOW);
      cancelled.status = MeetingStatus::Cancelled;
      let ended = meeting(11, 1, NOW - 2 * HOUR);
      let somebody_elses = meeting(12, 2, NOW);
      let ids = ids(admitting_candidates(
        vec![
          running,
          later_occurrence,
          soon,
          tomorrow,
          cancelled,
          ended,
          somebody_elses,
        ],
        id(1),
        None,
        NOW,
      ));
      assert_eq!(ids, vec![id(7), id(8)]);
    }

    #[test]
    fn somebody_elses_meeting_is_watched_only_while_in_its_call() {
      let theirs = meeting(7, 2, NOW);
      let other = meeting(8, 2, NOW);
      let in_call = Some(theirs.conversation_id);
      assert_eq!(
        ids(admitting_candidates(
          vec![theirs.clone(), other.clone()],
          id(1),
          in_call,
          NOW
        )),
        vec![id(7)]
      );
      assert!(admitting_candidates(vec![theirs, other], id(1), None, NOW).is_empty());
    }
  }

  mod handover {
    use super::*;

    #[test]
    fn a_meeting_handed_to_the_user_is_news_once() {
      let mut state = MeetingWatchState::default();
      let before = vec![meeting(7, 2, NOW + HOUR)];
      let after = vec![meeting(7, 1, NOW + HOUR)];
      assert!(
        state.handed_over(&before, id(1)).is_empty(),
        "the first list is no news"
      );
      assert_eq!(ids(state.handed_over(&after, id(1))), vec![id(7)]);
      assert!(state.handed_over(&after, id(1)).is_empty(), "said once");
    }

    #[test]
    fn being_made_a_co_host_signs_nothing() {
      let mut state = MeetingWatchState::default();
      let before = vec![meeting(7, 2, NOW + HOUR)];
      let mut after = before.clone();
      after[0].co_host_user_ids = vec![id(1)];
      state.handed_over(&before, id(1));
      assert!(state.handed_over(&after, id(1)).is_empty());
    }

    #[test]
    fn a_meeting_new_to_the_list_is_no_handover() {
      let mut state = MeetingWatchState::default();
      state.handed_over(&[meeting(8, 2, NOW)], id(1));
      assert!(
        state
          .handed_over(&[meeting(8, 2, NOW), meeting(7, 1, NOW)], id(1))
          .is_empty()
      );
    }
  }

  fn ids(meetings: Vec<MeetingResponse>) -> Vec<MeetingId> {
    meetings.iter().map(|meeting| meeting.id).collect()
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
      has_lobby: false,
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
