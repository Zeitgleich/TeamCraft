//! Wire types between a client core running outside the UI (the desktop shell's Rust side) and the
//! UI in the webview. The core owns the encrypted store and the network; the UI keeps a mirror of
//! what it shows and asks for more through these messages.

use serde::{Deserialize, Serialize};

use crate::calls::{ActiveCall, CallRequest, IncomingCall, MediaDevices, RunningCall};
use crate::model::{
  Conversation, ConversationId, CustomStatus, Message, MessageId, PresenceStatus, Space, SpaceId, ThreadId,
  User, UserId,
};

/// Event name the host emits whenever the local store changed (new messages, membership, presence).
pub const CORE_CHANGED_EVENT: &str = "core-changed";

/// Event name the host emits whenever call state changed (ringing, roster, media).
pub const CALL_CHANGED_EVENT: &str = "call-changed";

/// Everything the call UI shows, in one piece.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CallSnapshot {
  #[serde(default)]
  pub maybe_active: Option<ActiveCall>,
  pub incoming: Vec<IncomingCall>,
  /// Calls running in the user's conversations.
  pub running: Vec<RunningCall>,
  pub devices: MediaDevices,
}

/// Something the user did in or about a call.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum CallAction {
  Join {
    request: CallRequest,
  },
  Leave,
  Decline {
    conversation: ConversationId,
  },
  SetMuted {
    is_muted: bool,
  },
  SetDeafened {
    is_deafened: bool,
  },
  /// `None` follows the system default.
  SelectMicrophone {
    #[serde(default)]
    maybe_device: Option<String>,
  },
  SelectSpeakers {
    #[serde(default)]
    maybe_device: Option<String>,
  },
  #[serde(other)]
  Unknown,
}

/// HTTP verbs the UI may use for authenticated server calls (the shell attaches the bearer token).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiMethod {
  Get,
  Post,
  Put,
  Patch,
  Delete,
}

impl ApiMethod {
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Get => "GET",
      Self::Post => "POST",
      Self::Put => "PUT",
      Self::Patch => "PATCH",
      Self::Delete => "DELETE",
    }
  }
}

/// Everything the sidebar and headers need, in one piece.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChatSnapshot {
  pub me: User,
  pub users: Vec<User>,
  pub spaces: Vec<Space>,
  /// Channels of all spaces plus direct and group messages.
  pub conversations: Vec<Conversation>,
  /// Top-level messages per conversation, so the feed knows its length before loading any row.
  pub message_counts: Vec<(ConversationId, usize)>,
}

/// A slice of one conversation's feed, oldest first.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MessageWindow {
  pub conversation: ConversationId,
  pub start: usize,
  /// Length of the whole feed at the time of reading.
  pub total: usize,
  pub messages: Vec<Message>,
}

/// Where the local client stands.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CoreStatus {
  /// No signed-in account yet.
  NotStarted,
  /// First run on this device: keys have to be created or recovered.
  NeedsEnrollment {
    /// The account already has keys on the server, so the recovery passphrase unlocks them.
    has_existing_identity: bool,
  },
  Ready {
    /// Fingerprint of the account's identity key.
    identity_fingerprint: String,
  },
  Failed {
    message: String,
  },
}

/// First-run setup of this device (e2ee.md §3, §6).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnrollRequest {
  /// Wraps the key backup; recovers an existing identity when the account has one.
  pub passphrase: String,
  /// Shown in the user's device list.
  pub device_name: String,
}

/// Something the user did that changes data.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ChatAction {
  PostMessage {
    conversation: ConversationId,
    #[serde(default)]
    maybe_thread: Option<ThreadId>,
    body: String,
  },
  ToggleReaction {
    conversation: ConversationId,
    message: MessageId,
    emoji: String,
  },
  MarkRead {
    conversation: ConversationId,
  },
  SetFavorite {
    conversation: ConversationId,
    is_favorite: bool,
  },
  SetPresence {
    presence: PresenceStatus,
  },
  SetCustomStatus {
    #[serde(default)]
    maybe_status: Option<CustomStatus>,
  },
  /// Founds a new channel (and its encryption group) in a space.
  CreateChannel {
    space: SpaceId,
    name: String,
  },
  /// Opens, or finds, the direct message with one person.
  OpenDm {
    user: UserId,
  },
  /// Adds a person to a conversation and gives them the history the policy allows.
  AddMember {
    conversation: ConversationId,
    user: UserId,
  },
  /// Signs and publishes a workspace policy version. Owners and admins form the first signed roster
  /// when the workspace has none yet (the signer becomes the root of trust).
  PublishPolicy {
    policy: teamcraft_proto::WorkspacePolicy,
    #[serde(default)]
    owners: Vec<UserId>,
    #[serde(default)]
    admins: Vec<UserId>,
  },
  /// An admin adds themselves to a conversation (subject to the policy's `admin_self_join`).
  AdminJoinChannel {
    conversation: ConversationId,
  },
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::id_from_raw;

  #[test]
  fn actions_round_trip_as_tagged_json() {
    let action = ChatAction::PostMessage {
      conversation: id_from_raw(7),
      maybe_thread: None,
      body: "hi".into(),
    };
    let json = serde_json::to_string(&action).expect("serialize");
    assert_eq!(
      json,
      r#"{"action":"post_message","conversation":"7","maybe_thread":null,"body":"hi"}"#
    );
    assert_eq!(serde_json::from_str::<ChatAction>(&json).expect("parse"), action);
  }

  #[test]
  fn call_actions_are_tagged_and_unknown_ones_tolerated() {
    let action = CallAction::SetMuted { is_muted: true };
    let json = serde_json::to_string(&action).expect("serialize");
    assert_eq!(json, r#"{"action":"set_muted","is_muted":true}"#);
    assert_eq!(serde_json::from_str::<CallAction>(&json).expect("parse"), action);
    assert_eq!(
      serde_json::from_str::<CallAction>(r#"{"action":"raise_hand"}"#).expect("parse"),
      CallAction::Unknown
    );
    let join = CallAction::Join {
      request: CallRequest {
        conversation: id_from_raw(5),
        kind: crate::calls::CallKind::Direct,
      },
    };
    assert_eq!(
      serde_json::to_string(&join).expect("serialize"),
      r#"{"action":"join","request":{"conversation":"5","kind":"direct"}}"#
    );
  }

  #[test]
  fn status_is_tagged_by_state() {
    let status = CoreStatus::NeedsEnrollment {
      has_existing_identity: true,
    };
    assert_eq!(
      serde_json::to_string(&status).expect("serialize"),
      r#"{"state":"needs_enrollment","has_existing_identity":true}"#
    );
  }
}
