//! Scheduling, changing, cancelling and starting meetings in the apps (#1841), as desktop and web
//! do it (`teamcraft-ui`'s meetings view, docs/meetings.md): the server saves the meeting, then the
//! organizer's device sets up the meeting chat's encryption group with the invitees by where each
//! comes from (`InviteeList`, #1726), founded anchored with the meeting (#1747), and keeps the
//! signed admitters current (#1103). The apps show the form and the list; the decisions are the
//! core's (`teamcraft_client_api::meeting_edits`) and are made here.
//!
//! Robustness (robustness.md): a create or "Meet now" carries the form's request key, so a retry
//! after a lost answer makes no second meeting. An edit that takes people off notes them before the
//! save is sent ([`PENDING_EDITS_KEY`]): if the answer is lost or the app dies before the group
//! changed, the next [`Engine::meeting_upkeep`] finishes it from the list. The chat setup itself
//! notes its progress in the core and is finished on a later sync when cut short.

use std::collections::HashMap;

use teamcraft_client_api::backend::ChatBackend as _;
use teamcraft_client_api::meeting_edits::{
  ADMITTERS_SYNC_INTERVAL_MS, DraftProblem, MeetingDraft, PendingEdit, admitters_due, invited_users,
  invitee_change, noting_pending_edit, plan_reconcile,
};
use teamcraft_client_api::meeting_invitees::{ChatInvitees, added_users, attending_users, saved_change};
use teamcraft_client_api::meeting_schedule::{MeetingSchedule, MeetingTimes, ScheduleEdit, ScheduleNote};
use teamcraft_core::clock::{Clock as _, SystemClock};
use teamcraft_core::meeting_invitees::InviteeList;
use teamcraft_core::records;
use teamcraft_core::store::Table;
use teamcraft_core::ui_backend::CoreCommand;
use teamcraft_proto::{
  CancelScope, CreateInstantMeetingRequest, IdempotencyKey, ListMeetingsResponse, MeetingControlAction,
  MeetingControlRequest, MeetingId, MeetingResponse, SpaceId, UserId,
};

use crate::bridge::{from_json, to_json};
use crate::engine::{Engine, parse_id, parse_ids};
use crate::error::TeamCraftError;

/// Where the edits whose chat change is still owed are kept (`SyncState`).
const PENDING_EDITS_KEY: &str = "meeting_edits_pending";
/// How far back the upkeep's list reaches: meetings that ended this recently are still listed, as
/// on desktop.
const LIST_LOOKBACK_MS: i64 = 60 * 60_000;
const LIST_LIMIT: u32 = 100;

/// The schedule form of the apps: one meeting, invited members, the lobby. Repeats, e-mail
/// invitees, co-hosts and the notetaker stay on desktop and web for now; an edit keeps what the
/// meeting has of them.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct MeetingForm {
  pub title: String,
  pub description: String,
  pub starts_at_ms: i64,
  pub duration_minutes: u32,
  /// The invited members, the organizer left out.
  pub invitee_user_ids: Vec<String>,
  pub has_lobby: bool,
  /// Made with a new form and kept for every submit of it, so a retry after a lost answer
  /// schedules the meeting once. `None` in an edit's form.
  pub maybe_request_key: Option<String>,
}

/// Why a form can't be saved; the app shows each at its field.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum MeetingFormProblem {
  MissingTitle,
  StartInThePast,
  InvalidDuration,
}

/// A meeting the server saved, for the app to show and join.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SavedMeeting {
  pub meeting_id: String,
  pub conversation_id: String,
  pub title: String,
  pub starts_at_ms: i64,
  pub ends_at_ms: i64,
  /// The link for members of the workspace (guests get personal links).
  pub join_url: String,
}

/// How saving a form went.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum MeetingSave {
  Saved {
    meeting: SavedMeeting,
  },
  /// Nothing was sent: the form has these problems.
  Refused {
    problems: Vec<MeetingFormProblem>,
  },
}

/// What a cancel takes away.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum MeetingCancelScope {
  /// The whole meeting (the only scope of one that doesn't repeat).
  Series,
  /// One occurrence of a repeating meeting.
  Occurrence,
  /// The occurrence and every one after it.
  Following,
}

/// A meeting the user organizes, as its edit form opens.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct MeetingEditForm {
  pub form: MeetingForm,
  /// The meeting repeats: the form changes the series, the cancel asks which meetings.
  pub is_repeating: bool,
}

/// A new form: 30 minutes at the next full hour with some lead, with a lobby and a request key.
#[uniffi::export]
pub fn new_meeting_form(now_ms: i64) -> MeetingForm {
  form_of(&MeetingDraft::new(now_ms))
}

/// What keeps `form` from being saved now, in the order of the fields. `maybe_saved_start_ms` is
/// the saved start of a meeting being edited (unchanged, it may lie in the past).
#[uniffi::export]
pub fn meeting_form_problems(
  form: MeetingForm,
  now_ms: i64,
  maybe_saved_start_ms: Option<i64>,
) -> Vec<MeetingFormProblem> {
  match draft_of(&form) {
    Ok(draft) => problems_of(&draft.problems(now_ms, maybe_saved_start_ms)),
    // Malformed IDs are the app's bug, not the user's: the save reports them.
    Err(_) => Vec::new(),
  }
}

impl Engine {
  /// Schedules `form` (in the space `maybe_space_id`, where it is listed) and sets up its chat with
  /// the members invited here, from this request and never the server's member list.
  pub(crate) async fn schedule_meeting(
    &mut self,
    form: &MeetingForm,
    maybe_space_id: Option<&str>,
  ) -> Result<MeetingSave, TeamCraftError> {
    let draft = draft_of(form)?;
    let mut request = match draft.to_request(SystemClock.now_ms()) {
      Ok(request) => request,
      Err(problems) => {
        return Ok(MeetingSave::Refused {
          problems: problems_of(&problems),
        });
      },
    };
    request.maybe_space = maybe_space_id.map(parse_id::<SpaceId>).transpose()?;
    let meeting: MeetingResponse = from_json(
      &self
        .api_request("POST", "/meetings", Some(to_json(&request)?))
        .await?,
    )?;
    let invitees = InviteeList::Chosen {
      added: invited_users(&request),
      kept: Vec::new(),
    };
    self
      .run_commands(vec![
        CoreCommand::SetUpMeetingChat {
          conversation: meeting.conversation_id,
          meeting: meeting.id,
          invitees,
        },
        // The lobby as the organizer chose it here, never as the server reports it, decides
        // whether this device lets guests of the link in by itself (#987).
        CoreCommand::NoteMeetingLobby {
          meeting: meeting.id,
          has_lobby: request.has_lobby,
        },
        // The schedule as chosen here, never as the server reports it later, bounds the
        // admitters' validity (#1919).
        CoreCommand::NoteMeetingSchedule {
          meeting: meeting.id,
          note: ScheduleNote::Scheduled {
            schedule: MeetingSchedule::scheduled(&request),
          },
        },
        sync_admitters(&meeting),
      ])
      .await?;
    self.note_admitters_synced(meeting.id);
    Ok(MeetingSave::Saved {
      meeting: saved(&meeting),
    })
  }

  /// "Meet now": a meeting that starts at once, with nobody invited yet; its chat is set up with
  /// the organizer alone. `request_key` (hex, made once per tap) makes a retry return the same one.
  pub(crate) async fn start_meeting_now(
    &mut self,
    maybe_title: Option<String>,
    maybe_space_id: Option<&str>,
    request_key: &str,
  ) -> Result<SavedMeeting, TeamCraftError> {
    let request = CreateInstantMeetingRequest {
      maybe_title: maybe_title
        .map(|title| title.trim().to_owned())
        .filter(|title| !title.is_empty()),
      invitees: Vec::new(),
      maybe_space: maybe_space_id.map(parse_id::<SpaceId>).transpose()?,
      maybe_idempotency_key: Some(request_key_of(request_key)?),
    };
    let meeting: MeetingResponse = from_json(
      &self
        .api_request("POST", "/meetings/instant", Some(to_json(&request)?))
        .await?,
    )?;
    self
      .run_commands(vec![
        CoreCommand::SetUpMeetingChat {
          conversation: meeting.conversation_id,
          meeting: meeting.id,
          invitees: InviteeList::Chosen {
            added: Vec::new(),
            kept: Vec::new(),
          },
        },
        // The times the server gave the meeting just started, bounded, are its schedule from here
        // (#1919).
        CoreCommand::NoteMeetingSchedule {
          meeting: meeting.id,
          note: ScheduleNote::Started {
            server_times: MeetingTimes::of(&meeting),
          },
        },
        sync_admitters(&meeting),
      ])
      .await?;
    self.note_admitters_synced(meeting.id);
    Ok(saved(&meeting))
  }

  /// The edit form of the meeting `meeting_id`, as the server has it now; only its organizer edits.
  pub(crate) async fn meeting_edit_form(&self, meeting_id: &str) -> Result<MeetingEditForm, TeamCraftError> {
    let me = self.backend()?.me().id;
    let meeting = self.own_meeting(meeting_id, me).await?;
    Ok(MeetingEditForm {
      form: form_of(&MeetingDraft::from_meeting(&meeting, me)),
      is_repeating: meeting.maybe_recurrence.is_some(),
    })
  }

  /// Saves an edit and, once the server took it, brings the meeting chat's group in line: the
  /// members the organizer added here go in as its own choice, those carried over only where the
  /// signed list names them, and those taken off are removed with a key-rotating commit (e2ee.md
  /// §4.6, "Meeting chats").
  pub(crate) async fn save_meeting_edit(
    &mut self,
    meeting_id: &str,
    form: &MeetingForm,
  ) -> Result<MeetingSave, TeamCraftError> {
    let me = self.backend()?.me().id;
    // Diffed against the meeting as the server has it now, not a list the app showed earlier.
    let before = self.own_meeting(meeting_id, me).await?;
    let draft = draft_of(form)?;
    let request = match draft.to_update_request(SystemClock.now_ms(), &before) {
      Ok(request) => request,
      Err(problems) => {
        return Ok(MeetingSave::Refused {
          problems: problems_of(&problems),
        });
      },
    };
    let added = added_users(&request, &[&before]);
    // Written down before the save: if the answer is lost or the app dies before the chat's group
    // lost the removed users, the upkeep finishes the removal from the list.
    if let Some((_, removed)) = invitee_change(&before, &request) {
      self.note_pending_edit(before.id, removed)?;
    }
    let path = format!("/meetings/{}", before.id);
    let saved_meeting: MeetingResponse =
      from_json(&self.api_request("PATCH", &path, Some(to_json(&request)?)).await?)?;
    let (invitees, removed) = saved_change(&before, &saved_meeting, added);
    let mut commands = Vec::new();
    if let Some(has_lobby) = request.maybe_has_lobby {
      commands.push(CoreCommand::NoteMeetingLobby {
        meeting: saved_meeting.id,
        has_lobby,
      });
    }
    if let Some(edit) = ScheduleEdit::of(&before, &request) {
      commands.push(CoreCommand::NoteMeetingSchedule {
        meeting: saved_meeting.id,
        note: ScheduleNote::Edited { edit },
      });
    }
    commands.push(CoreCommand::UpdateMeetingChatMembers {
      conversation: saved_meeting.conversation_id,
      meeting: saved_meeting.id,
      invitees: InviteeList::from(invitees),
      removed,
    });
    commands.push(sync_admitters(&saved_meeting));
    self.run_commands(commands).await?;
    self.note_admitters_synced(saved_meeting.id);
    // The core keeps the removal from here on (`update_meeting_chat_members`).
    self.clear_pending_edit(saved_meeting.id)?;
    Ok(MeetingSave::Saved {
      meeting: saved(&saved_meeting),
    })
  }

  /// "End for everyone" from the call (#1930): ends the meeting, whether or not a call runs, and
  /// stops its call (#1931). Hosts only; the server checks who hosts.
  pub(crate) async fn end_meeting_for_everyone(&self, meeting_id: &str) -> Result<(), TeamCraftError> {
    let meeting: MeetingId = parse_id(meeting_id)?;
    let request = MeetingControlRequest {
      action: MeetingControlAction::End,
    };
    self
      .api_request(
        "POST",
        &format!("/meetings/{meeting}/controls"),
        Some(to_json(&request)?),
      )
      .await?;
    Ok(())
  }

  /// Cancels the meeting, or of a repeating one the occurrence starting at `occurrence_ms` (and
  /// with [`MeetingCancelScope::Following`] every one after it). The chat stays; the server tells
  /// everybody.
  pub(crate) async fn cancel_meeting(
    &self,
    meeting_id: &str,
    scope: MeetingCancelScope,
    occurrence_ms: i64,
  ) -> Result<(), TeamCraftError> {
    let meeting: MeetingId = parse_id(meeting_id)?;
    let path = match scope {
      MeetingCancelScope::Series => format!("/meetings/{meeting}"),
      MeetingCancelScope::Occurrence => {
        format!(
          "/meetings/{meeting}?scope={}&occurrence={occurrence_ms}",
          CancelScope::Occurrence
        )
      },
      MeetingCancelScope::Following => {
        format!(
          "/meetings/{meeting}?scope={}&occurrence={occurrence_ms}",
          CancelScope::Following
        )
      },
    };
    self.api_request("DELETE", &path, None).await?;
    Ok(())
  }

  /// Before the user joins the call of `meeting_id`: its organizer's device founds the chat's group
  /// where a meeting from before 1.0.3 has none, with the server's attendees of whom only those the
  /// organizer's signed list names go in (#1726); anybody else knocks at the lobby. Nothing happens
  /// for somebody else's meeting, or when the group is there.
  pub(crate) async fn prepare_meeting_join(&mut self, meeting_id: &str) -> Result<(), TeamCraftError> {
    let me = self.backend()?.me().id;
    let meeting: MeetingResponse = from_json(
      &self
        .api_request("GET", &format!("/meetings/{meeting_id}"), None)
        .await?,
    )?;
    if meeting.organizer_user_id != me {
      return Ok(());
    }
    self
      .run_commands(vec![CoreCommand::SetUpMeetingChat {
        conversation: meeting.conversation_id,
        meeting: meeting.id,
        invitees: InviteeList::from(ChatInvitees::listed(attending_users(&meeting))),
      }])
      .await
  }

  /// The organizer's upkeep, as desktop runs it with every meeting list: the signed admitters of
  /// the user's meetings that run or start within a day are compared (at most every ten minutes
  /// per meeting), and edits whose chat change is still owed are finished. Returns the list it read
  /// (`GET /meetings`, JSON), so the app shows what the upkeep worked on.
  pub(crate) async fn meeting_upkeep(&mut self) -> Result<String, TeamCraftError> {
    let me = self.backend()?.me().id;
    let now = SystemClock.now_ms();
    let path = format!("/meetings?from={}&limit={LIST_LIMIT}", now - LIST_LOOKBACK_MS);
    let json = self.api_request("GET", &path, None).await?;
    let list: ListMeetingsResponse = from_json(&json)?;
    let mut commands = Vec::new();
    for due in admitters_due(&list.meetings, me, now) {
      if self
        .meeting_admitters_synced
        .get(&due.meeting)
        .is_some_and(|at| now - at < ADMITTERS_SYNC_INTERVAL_MS)
      {
        continue;
      }
      self.meeting_admitters_synced.insert(due.meeting, now);
      commands.push(CoreCommand::SyncMeetingAdmitters {
        meeting: due.meeting,
        conversation: due.conversation,
        server_times: due.server_times,
      });
    }
    let edits = self.pending_edits()?;
    let (kept, owed) = plan_reconcile(&edits, now, &list.meetings);
    for change in owed {
      commands.push(CoreCommand::UpdateMeetingChatMembers {
        conversation: change.conversation,
        meeting: change.meeting,
        // The server's list: only users the organizer's signed list names are added (#1726).
        invitees: InviteeList::Listed(change.attending),
        removed: change.removed,
      });
    }
    if !commands.is_empty() {
      self.run_commands(commands).await?;
    }
    if kept != edits {
      self.store_pending_edits(&kept)?;
    }
    Ok(json)
  }

  /// The meeting `meeting_id` if `me` organizes it.
  async fn own_meeting(&self, meeting_id: &str, me: UserId) -> Result<MeetingResponse, TeamCraftError> {
    let meeting: MeetingId = parse_id(meeting_id)?;
    let found: MeetingResponse = from_json(
      &self
        .api_request("GET", &format!("/meetings/{meeting}"), None)
        .await?,
    )?;
    if found.organizer_user_id != me {
      return Err(TeamCraftError::InvalidInput(
        "only the organizer changes a meeting".into(),
      ));
    }
    Ok(found)
  }

  fn note_admitters_synced(&mut self, meeting: MeetingId) {
    self
      .meeting_admitters_synced
      .insert(meeting, SystemClock.now_ms());
  }

  fn pending_edits(&self) -> Result<Vec<PendingEdit>, TeamCraftError> {
    Ok(records::get_json(&self.store, Table::SyncState, PENDING_EDITS_KEY)?.unwrap_or_default())
  }

  fn store_pending_edits(&self, edits: &[PendingEdit]) -> Result<(), TeamCraftError> {
    Ok(records::put_json(
      &self.store,
      Table::SyncState,
      PENDING_EDITS_KEY,
      &edits,
    )?)
  }

  fn note_pending_edit(&self, meeting: MeetingId, removed: Vec<UserId>) -> Result<(), TeamCraftError> {
    if removed.is_empty() {
      return Ok(());
    }
    let edits = noting_pending_edit(self.pending_edits()?, meeting, removed, SystemClock.now_ms());
    self.store_pending_edits(&edits)
  }

  fn clear_pending_edit(&self, meeting: MeetingId) -> Result<(), TeamCraftError> {
    let mut edits = self.pending_edits()?;
    let before = edits.len();
    edits.retain(|edit| edit.meeting != meeting);
    if edits.len() != before {
      self.store_pending_edits(&edits)?;
    }
    Ok(())
  }
}

/// The admitters statement of `meeting` as the organizer's device signs it after a save.
fn sync_admitters(meeting: &MeetingResponse) -> CoreCommand {
  CoreCommand::SyncMeetingAdmitters {
    meeting: meeting.id,
    conversation: meeting.conversation_id,
    server_times: MeetingTimes::of(meeting),
  }
}

fn saved(meeting: &MeetingResponse) -> SavedMeeting {
  SavedMeeting {
    meeting_id: meeting.id.to_string(),
    conversation_id: meeting.conversation_id.to_string(),
    title: meeting.title.clone(),
    starts_at_ms: meeting.starts_at.0,
    ends_at_ms: meeting.ends_at.0,
    join_url: meeting.join_url.clone(),
  }
}

fn form_of(draft: &MeetingDraft) -> MeetingForm {
  MeetingForm {
    title: draft.title.clone(),
    description: draft.description.clone(),
    starts_at_ms: draft.starts_at,
    duration_minutes: draft.duration_minutes,
    invitee_user_ids: draft.members.iter().map(ToString::to_string).collect(),
    has_lobby: draft.has_lobby,
    maybe_request_key: draft.maybe_idempotency_key.map(|key| key.to_hex()),
  }
}

fn draft_of(form: &MeetingForm) -> Result<MeetingDraft, TeamCraftError> {
  Ok(MeetingDraft {
    title: form.title.clone(),
    description: form.description.clone(),
    starts_at: form.starts_at_ms,
    duration_minutes: form.duration_minutes,
    members: parse_ids(&form.invitee_user_ids)?,
    has_lobby: form.has_lobby,
    maybe_idempotency_key: form
      .maybe_request_key
      .as_deref()
      .map(request_key_of)
      .transpose()?,
  })
}

fn request_key_of(text: &str) -> Result<IdempotencyKey, TeamCraftError> {
  IdempotencyKey::from_hex(text).map_err(|_| TeamCraftError::InvalidInput("not a request key".to_owned()))
}

fn problems_of(problems: &[DraftProblem]) -> Vec<MeetingFormProblem> {
  problems
    .iter()
    .map(|problem| match problem {
      DraftProblem::MissingTitle => MeetingFormProblem::MissingTitle,
      DraftProblem::StartInThePast => MeetingFormProblem::StartInThePast,
      DraftProblem::InvalidDuration => MeetingFormProblem::InvalidDuration,
    })
    .collect()
}

/// When the upkeep last had the core compare each meeting's signed admitters, on this engine.
pub(crate) type AdmittersSyncedAt = HashMap<MeetingId, i64>;

#[cfg(test)]
mod tests {
  use super::*;

  const NOW: i64 = 1_800_000_000_000;

  #[test]
  fn a_new_form_carries_a_request_key_and_needs_a_title() {
    let form = new_meeting_form(NOW);
    assert!(
      form
        .maybe_request_key
        .as_deref()
        .is_some_and(|key| key.len() == 32)
    );
    assert!(form.starts_at_ms > NOW);
    assert_eq!(
      meeting_form_problems(form.clone(), NOW, None),
      vec![MeetingFormProblem::MissingTitle]
    );
    let draft = draft_of(&form).expect("readable");
    assert_eq!(form_of(&draft), form, "the form goes through the core unchanged");
  }

  #[test]
  fn a_form_in_the_past_is_refused_unless_it_is_the_saved_start() {
    let mut form = new_meeting_form(NOW);
    form.title = "Planning".into();
    form.starts_at_ms = NOW - 30 * 60_000;
    assert_eq!(
      meeting_form_problems(form.clone(), NOW, None),
      vec![MeetingFormProblem::StartInThePast]
    );
    assert!(meeting_form_problems(form, NOW, Some(NOW - 30 * 60_000)).is_empty());
  }

  #[test]
  fn malformed_ids_and_keys_are_the_apps_bug() {
    let mut form = new_meeting_form(NOW);
    form.invitee_user_ids = vec!["nope".into()];
    assert!(matches!(draft_of(&form), Err(TeamCraftError::InvalidInput(_))));
    let mut form = new_meeting_form(NOW);
    form.maybe_request_key = Some("short".into());
    assert!(matches!(draft_of(&form), Err(TeamCraftError::InvalidInput(_))));
  }
}
