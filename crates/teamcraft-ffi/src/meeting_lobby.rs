//! A meeting's lobby in the phone apps (#1859), as desktop and web have it (`teamcraft-ui`'s
//! lobby, docs/meetings.md "Lobby"): the hosts of a meeting see who waits during its call and admit
//! or deny them, guests of a guest link included. Admitting lets the person's devices into the
//! meeting chat's encryption on this host's own decision (`Client::let_in_from_lobby`, e2ee.md
//! §4.6, "Meeting guests"): a guest only with the key their page reported, and only from a device
//! the organizer's key, the signed host list or the signed admitters name. The server's lobby is
//! the list of who knocked, never the say on who gets in.
//!
//! Also the organizer who left the call from the system's call UI with people still waiting: what
//! the reminder offers to hand the meeting to.

use teamcraft_client_api::backend::ChatBackend as _;
use teamcraft_client_api::meeting_hosts::{MeetingRole, role_of, should_offer_handover};
use teamcraft_client_api::meeting_invitees::GUEST_ACCESS_GRACE_MS;
use teamcraft_core::meeting_lobby::{LobbyAdmission, SignedMembership};
use teamcraft_proto::{ConversationId, ListLobbyResponse, LobbyEntry, MeetingResponse, UserId};

use crate::bridge::from_json;
use crate::engine::{Engine, parse_id};
use crate::error::TeamCraftError;

/// How a waiting person can be let in, for their row in the lobby. Who is a member is the owners'
/// signed roster's word, never the server's user kind (#1873).
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum LobbyPersonStatus {
  /// A member of the workspace by the signed roster.
  Member,
  /// A guest whose device is set up: "Admit" adds it. `maybe_key_code` is the code of the key this
  /// device pinned for them, to compare with the guest's screen; `None` until it is made (read
  /// the lobby again).
  Guest { maybe_key_code: Option<String> },
  /// A guest whose page is still setting up its device: no "Admit" yet.
  GuestDevicePending,
  /// The server calls them a member, but the signed roster doesn't vouch for them yet: no
  /// "Admit" (the row says so; inviting them to the meeting is the way in).
  UnconfirmedMember,
}

/// Somebody waiting in the lobby.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct LobbyPerson {
  pub user_id: String,
  pub display_name: String,
  pub status: LobbyPersonStatus,
  /// Whether "Admit" can be offered: a member at once, a guest only once their key code is shown
  /// (`LobbyAdmission::can_admit`, the rule every client's lobby shares).
  pub can_admit: bool,
}

/// The lobby of a meeting the user hosts.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct MeetingLobby {
  pub meeting_id: String,
  pub people: Vec<LobbyPerson>,
}

/// A host's answer to somebody waiting.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum LobbyDecision {
  Admit,
  Deny,
}

/// How a decision went.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum LobbyDecisionOutcome {
  /// Admitted (and let into the chat's encryption) or denied.
  Done,
  /// They no longer wait: another host decided, or they left.
  NoLongerWaiting,
}

/// The organizer left the call while people wait in the lobby and nobody else hosts the meeting:
/// whom it can be handed to (the members still in the call).
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct LobbyLeftWaiting {
  pub meeting_title: String,
  pub waiting: u32,
  pub candidate_user_ids: Vec<String>,
}

/// A host's read of the lobby: the meeting and who waits.
pub(crate) struct LobbyRead {
  pub(crate) meeting: MeetingResponse,
  pub(crate) entries: Vec<LobbyEntry>,
}

impl Engine {
  /// The lobby of the meeting whose chat is `conversation_id`, if the user hosts it; `None` for a
  /// conversation of no meeting and for anybody who doesn't host it (the server refuses them too).
  pub(crate) async fn lobby_read(&self, conversation_id: &str) -> Result<Option<LobbyRead>, TeamCraftError> {
    let conversation: ConversationId = parse_id(conversation_id)?;
    let me = self.backend()?.me().id;
    let Some(meeting) = self
      .meeting_of(conversation)
      .await?
      .filter(|meeting| is_lobby_host(meeting, me))
    else {
      return Ok(None);
    };
    let lobby = self.lobby_of(&meeting).await?;
    Ok(lobby.is_host.then_some(LobbyRead {
      meeting,
      entries: lobby.entries,
    }))
  }

  /// Admits or denies `user_id` waiting in the lobby of the meeting whose chat is
  /// `conversation_id`. The entry is read afresh: somebody who no longer waits is no error. An
  /// admitted person's devices are let into the chat's encryption from this device, which fails
  /// for a device that isn't a host's by the signed lists or a guest key other than the reported.
  pub(crate) async fn decide_lobby_entry(
    &mut self,
    conversation_id: &str,
    user_id: &str,
    decision: LobbyDecision,
  ) -> Result<LobbyDecisionOutcome, TeamCraftError> {
    let user: UserId = parse_id(user_id)?;
    let Some(read) = self.lobby_read(conversation_id).await? else {
      return Err(TeamCraftError::InvalidInput(
        "the user doesn't host this meeting".into(),
      ));
    };
    let Some(entry) = read.entries.into_iter().find(|entry| entry.user.id == user) else {
      return Ok(LobbyDecisionOutcome::NoLongerWaiting);
    };
    // The rule the rows show, checked here too: a guest only once this device made the code of the
    // key it pinned for them, which the app showed for the host to compare.
    let backend = self.backend()?;
    let has_key_code = backend.guest_key_code(user).is_some();
    let membership = signed_membership(backend.is_signed_member(user));
    if decision == LobbyDecision::Admit && !LobbyAdmission::of(&entry, membership).can_admit(has_key_code) {
      return Err(TeamCraftError::InvalidInput(
        "the person can't be admitted yet: no key code shown or not a confirmed member".into(),
      ));
    }
    let meeting = read.meeting;
    let verb = match decision {
      LobbyDecision::Admit => "admit",
      LobbyDecision::Deny => "deny",
    };
    let path = format!("/meetings/{}/lobby/{user}/{verb}", meeting.id);
    self.api_request("POST", &path, None).await?;
    if decision == LobbyDecision::Admit {
      // The meeting's end as the server lists it; the core caps a guest's access by the
      // organizer's signed admitters (#1873).
      let (chat, until) = (meeting.conversation_id, meeting.ends_at.0 + GUEST_ACCESS_GRACE_MS);
      self
        .with_client(move |client| {
          Box::pin(async move { client.let_in_from_lobby(chat, &entry, until).await })
        })
        .await?;
    }
    Ok(LobbyDecisionOutcome::Done)
  }

  /// After the user left the call of `conversation_id` from the system's call UI: people wait in
  /// the meeting's lobby, the user organizes it, nobody else hosts it and a member still in the
  /// call could take it over. `None` otherwise, then there is nothing to remind of.
  pub(crate) async fn lobby_left_waiting(
    &self,
    conversation_id: &str,
  ) -> Result<Option<LobbyLeftWaiting>, TeamCraftError> {
    let conversation: ConversationId = parse_id(conversation_id)?;
    let backend = self.backend()?;
    let me = backend.me().id;
    let Some(meeting) = self
      .meeting_of(conversation)
      .await?
      .filter(|meeting| role_of(meeting, me) == MeetingRole::Organizer)
    else {
      return Ok(None);
    };
    let candidates: Vec<UserId> = self
      .call_participants(conversation_id)
      .await?
      .iter()
      .filter_map(|id| parse_id::<UserId>(id).ok())
      .filter(|id| {
        backend
          .user(*id)
          .is_some_and(|user| !user.is_guest && !user.is_bot)
      })
      .collect();
    if candidates.is_empty() {
      return Ok(None);
    }
    let waiting = self.lobby_of(&meeting).await?.entries.len();
    Ok(
      should_offer_handover(&meeting, me, waiting, &candidates).then(|| LobbyLeftWaiting {
        meeting_title: meeting.title.clone(),
        waiting: u32::try_from(waiting).unwrap_or(u32::MAX),
        candidate_user_ids: candidates.iter().map(ToString::to_string).collect(),
      }),
    )
  }

  async fn lobby_of(&self, meeting: &MeetingResponse) -> Result<ListLobbyResponse, TeamCraftError> {
    let path = format!("/meetings/{}/lobby", meeting.id);
    from_json(&self.api_request("GET", &path, None).await?)
  }
}

/// What the signed roster says of a waiting person, from the backend's answer.
pub(crate) fn signed_membership(is_signed_member: bool) -> SignedMembership {
  if is_signed_member {
    SignedMembership::Member
  } else {
    SignedMembership::NotSigned
  }
}

/// A host's row for `entry`, with what the signed roster says of them and the key code this device
/// made for a guest, if any yet.
pub(crate) fn lobby_person(
  entry: &LobbyEntry,
  membership: SignedMembership,
  maybe_key_code: Option<String>,
) -> LobbyPerson {
  let admission = LobbyAdmission::of(entry, membership);
  let can_admit = admission.can_admit(maybe_key_code.is_some());
  let status = match admission {
    LobbyAdmission::Member => LobbyPersonStatus::Member,
    LobbyAdmission::Guest { .. } => LobbyPersonStatus::Guest { maybe_key_code },
    LobbyAdmission::GuestDevicePending => LobbyPersonStatus::GuestDevicePending,
    LobbyAdmission::UnconfirmedMember => LobbyPersonStatus::UnconfirmedMember,
  };
  LobbyPerson {
    user_id: entry.user.id.to_string(),
    display_name: entry.user.display_name.clone(),
    status,
    can_admit,
  }
}

/// Whether the lobby of `meeting` is the user's to decide on, as the meeting list says: the
/// organizer, a co-host, or a chat admin the server lets host it. The server answers for the rest.
fn is_lobby_host(meeting: &MeetingResponse, me: UserId) -> bool {
  (role_of(meeting, me) != MeetingRole::Participant || meeting.can_host_lobby)
    && meeting.status == teamcraft_proto::MeetingStatus::Scheduled
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{Id, MeetingKind, MeetingStatus, Snowflake, UnixMillis};

  use super::*;

  #[test]
  fn only_hosts_of_a_running_meeting_see_its_lobby() {
    let mut meeting = meeting();
    assert!(is_lobby_host(&meeting, id(1)), "the organizer");
    assert!(is_lobby_host(&meeting, id(2)), "a co-host");
    assert!(!is_lobby_host(&meeting, id(3)), "a participant");
    meeting.can_host_lobby = true;
    assert!(
      is_lobby_host(&meeting, id(3)),
      "a chat admin the server lets host it"
    );
    meeting.status = MeetingStatus::Cancelled;
    assert!(!is_lobby_host(&meeting, id(1)));
  }

  #[test]
  fn a_row_says_how_somebody_can_be_let_in() {
    let not_signed = SignedMembership::NotSigned;
    let guest = entry("guest", true, Some("ab"));
    let shown = lobby_person(&guest, not_signed, Some("12 34".into()));
    assert_eq!(
      shown.status,
      LobbyPersonStatus::Guest {
        maybe_key_code: Some("12 34".into())
      }
    );
    assert!(shown.can_admit);
    assert!(
      !lobby_person(&guest, not_signed, None).can_admit,
      "no Admit before the key code is shown"
    );
    assert_eq!(
      lobby_person(&entry("guest", false, None), not_signed, None).status,
      LobbyPersonStatus::GuestDevicePending
    );
    let member = lobby_person(&entry("member", false, None), SignedMembership::Member, None);
    assert_eq!(member.status, LobbyPersonStatus::Member);
    assert!(member.can_admit);
    let unconfirmed = lobby_person(&entry("member", false, None), not_signed, None);
    assert_eq!(unconfirmed.status, LobbyPersonStatus::UnconfirmedMember);
    assert!(
      !unconfirmed.can_admit,
      "the server's word alone admits nobody (#1873)"
    );
    let relabelled = lobby_person(&entry("member", true, Some("ab")), not_signed, None);
    assert_eq!(
      relabelled.status,
      LobbyPersonStatus::Guest { maybe_key_code: None },
      "a guest the server calls a member"
    );
    assert_eq!(member.display_name, "Gus");
    assert_eq!(member.user_id, "77");
  }

  fn id<T: Id>(raw: i64) -> T {
    T::from_snowflake(Snowflake::new(raw).unwrap())
  }

  fn entry(kind: &str, has_device: bool, maybe_fingerprint: Option<&str>) -> LobbyEntry {
    serde_json::from_value(serde_json::json!({
      "user": {"id": "77", "display_name": "Gus", "kind": kind},
      "requested_at": 1,
      "is_by_link": true,
      "has_device": has_device,
      "knock_fingerprint": maybe_fingerprint,
    }))
    .unwrap()
  }

  fn meeting() -> MeetingResponse {
    MeetingResponse {
      id: id(7),
      title: "Planning".into(),
      maybe_description: None,
      starts_at: UnixMillis(0),
      ends_at: UnixMillis(1),
      kind: MeetingKind::Scheduled,
      status: MeetingStatus::Scheduled,
      has_lobby: true,
      organizer_user_id: id(1),
      conversation_id: id(70),
      maybe_space: None,
      join_url: String::new(),
      maybe_recurrence: None,
      is_occurrence_cancelled: false,
      is_occurrence_holiday: false,
      can_host_lobby: false,
      co_host_user_ids: vec![id(2)],
      attendees: Vec::new(),
      undelivered_invitations: Vec::new(),
    }
  }
}
