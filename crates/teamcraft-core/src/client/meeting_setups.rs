//! A meeting chat's setup this device owes (#2258). The server creates a meeting and its chat
//! without a group; the organizer's device then founds the group (`set_up_meeting_chat_from`),
//! which notes its own progress once it runs. Between the server's answer and that run the setup
//! waits in the app's command queue, and a process that dies there (the phone app killed right
//! after "Schedule") lost it: the chat stayed without a group until the organizer joined its call.
//!
//! So whoever queues the setup notes it first ([`note_meeting_setup_owed`], in the same thread as
//! the queueing, before anything can be lost), the setup clears the note once it went through, and
//! every sync pass finishes the setups still owed ([`Client::finish_owed_meeting_setups`]). The
//! setup is idempotent: running it again for a group that holds everybody asks nothing.

use serde::{Deserialize, Serialize};
use teamcraft_proto::{ConversationId, MeetingId};

use super::Client;
use crate::api::ServerApi;
use crate::error::CoreError;
use crate::meeting_invitees::InviteeList;
use crate::records;
use crate::store::{LocalStore, Table};

/// Key prefix of the owed setups in [`Table::SyncState`], by the meeting chat.
const OWED_PREFIX: &str = "meeting-setup-owed/";

/// A setup queued and not finished yet: what the queued command carries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct OwedSetup {
  meeting: MeetingId,
  invitees: InviteeList,
}

/// Notes that the setup of the meeting chat `conversation` for `meeting` with `invitees` is owed.
/// Called right before the setup command is queued.
pub fn note_meeting_setup_owed<S: LocalStore>(
  store: &S,
  conversation: ConversationId,
  meeting: MeetingId,
  invitees: &InviteeList,
) -> Result<(), CoreError> {
  let owed = OwedSetup {
    meeting,
    invitees: invitees.clone(),
  };
  records::put_json(store, Table::SyncState, &owed_key(conversation), &owed)
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Runs the meeting chat setups still owed (see the module), each once per call. A setup that
  /// went through, or can't ever (the conversation is no meeting chat, or gone), is no longer
  /// owed; one that failed on the network stays for the next sync, which the error fails.
  pub(crate) async fn finish_owed_meeting_setups(&mut self) -> Result<(), CoreError> {
    let owed: Vec<(String, OwedSetup)> =
      records::list_json_with_prefix(&self.store, Table::SyncState, OWED_PREFIX)?;
    let mut maybe_first_error = None;
    for (key, setup) in owed {
      let Some(conversation) = key
        .strip_prefix(OWED_PREFIX)
        .and_then(|id| id.parse::<ConversationId>().ok())
      else {
        self.store.delete(Table::SyncState, &key)?;
        continue;
      };
      let result =
        Box::pin(self.set_up_meeting_chat_from(conversation, Some(setup.meeting), &setup.invitees)).await;
      match result {
        Ok(_) => self.clear_meeting_setup_owed(conversation)?,
        Err(error) if error.is_transient() => {
          maybe_first_error.get_or_insert(error);
        },
        Err(error) => {
          // The setup's own record (noted once it ran) finishes what it started; nothing more is
          // owed from the queue.
          tracing::warn!(conversation_id = %conversation, %error, "an owed meeting chat setup can't be finished");
          self.clear_meeting_setup_owed(conversation)?;
        },
      }
    }
    maybe_first_error.map_or(Ok(()), Err)
  }

  /// The setup of `conversation` went through: nothing is owed any more.
  pub(crate) fn clear_meeting_setup_owed(&self, conversation: ConversationId) -> Result<(), CoreError> {
    self.store.delete(Table::SyncState, &owed_key(conversation))?;
    Ok(())
  }
}

fn owed_key(conversation: ConversationId) -> String {
  format!("{OWED_PREFIX}{conversation}")
}
