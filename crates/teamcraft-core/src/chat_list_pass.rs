//! The mobile apps' chat list in slices (issue #1819). A pass over every conversation held the main
//! read thread for 1.7–2.7 s on phones, at start and after every reconnect, and every read queued
//! behind it waited that long. A [`ListPass`] reads what all entries share once when it begins, then
//! builds the entries a slice at a time ([`CoreChatBackend::continue_list_pass`]): the read thread
//! runs what queued meanwhile between two slices.
//!
//! Each slice is one look at the store of its own: an entry is consistent in itself, while two
//! entries may come from looks a few milliseconds apart. A change meanwhile is followed by the
//! app's next ask, as before.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use teamcraft_client_api::model::{Conversation, SpaceId, TimestampMs, UserId};
use teamcraft_proto::{
  ConversationKind as ConversationKindProto, ConversationResponse, SpaceResponse, UserSummary,
};

use crate::account_data::AccountData;
use crate::notify_rules::MentionPreferences;
use crate::records;
use crate::store::{LocalStore, Table};
use crate::ui_backend::{CoreChatBackend, EntryDetail, has_channel_in};

/// How long a slice of a pass runs at most before it lets other reads in: well under the ~100 ms a
/// read may wait on a mid-range phone (issue #1819). A slice builds one entry at least.
pub const DEFAULT_SLICE_MS: i64 = 40;

/// A chat list pass under way: what the entries share, the conversations still to build, and the
/// entries built so far, in the store's order.
pub struct ListPass {
  spaces: Vec<ListedSpace>,
  remaining: VecDeque<ConversationResponse>,
  listed: Vec<Conversation>,
  /// The people the entries built so far needed: the user, and the people of direct messages.
  /// Read one by one as needed, not the whole users table per pass.
  users: BTreeMap<UserId, UserSummary>,
  preferences: MentionPreferences,
  account: AccountData,
  limit: SliceLimit,
  slices: usize,
}

/// A space as the chat list names its conversations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListedSpace {
  pub id: SpaceId,
  pub name: String,
  pub maybe_image_version: Option<String>,
}

/// When a slice of a pass ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SliceLimit {
  /// Time on the backend's clock after which the slice ends (it builds one entry at least).
  pub max_ms: i64,
  /// Entries a slice builds at most; `usize::MAX` for no count.
  pub max_entries: usize,
}

impl Default for SliceLimit {
  fn default() -> Self {
    Self {
      max_ms: DEFAULT_SLICE_MS,
      max_entries: usize::MAX,
    }
  }
}

impl SliceLimit {
  /// A pass in one slice: for callers that need the whole list at once.
  pub const WHOLE: Self = Self {
    max_ms: i64::MAX,
    max_entries: usize::MAX,
  };
}

impl ListPass {
  /// The spaces whose conversations the pass lists: the user's, and those the user has a channel
  /// in (moved there with its members), in the store's order.
  pub fn spaces(&self) -> &[ListedSpace] {
    &self.spaces
  }

  /// Whether every entry is built.
  pub fn is_done(&self) -> bool {
    self.remaining.is_empty()
  }

  /// How many slices ran so far.
  pub fn slices(&self) -> usize {
    self.slices
  }

  /// How many entries are still to build.
  pub fn remaining(&self) -> usize {
    self.remaining.len()
  }

  /// The entries, in the store's order; those not built yet are missing.
  pub fn into_listed(self) -> Vec<Conversation> {
    self.listed
  }

  /// The entries built since the last call, in the store's order: a caller that turns entries
  /// into its own items does so a slice at a time.
  pub fn take_listed(&mut self) -> Vec<Conversation> {
    std::mem::take(&mut self.listed)
  }
}

impl<S: LocalStore> CoreChatBackend<S> {
  /// A whole pass in one go ([`Self::begin_list_pass`]), for callers that need the list at once.
  pub fn whole_list_pass(&self) -> ListPass {
    let mut pass = self.begin_list_pass(SliceLimit::WHOLE);
    while !pass.is_done() {
      self.continue_list_pass(&mut pass);
    }
    pass
  }

  /// Starts a pass over the conversations of the listed spaces and every direct message, as the
  /// mobile apps' lists show them: a channel's entry carries no member list and no entry carries
  /// bookmarks. Reads the spaces, the conversation list, the mention rules and the account data,
  /// nothing per conversation yet.
  pub fn begin_list_pass(&self, limit: SliceLimit) -> ListPass {
    let _step = crate::timing::step("chat_list_begin");
    let conversations = self.conversations_raw();
    let stored: Vec<SpaceResponse> = self.read(records::list_json(self.store_ref(), Table::Spaces));
    let spaces: Vec<ListedSpace> = stored
      .into_iter()
      .filter(|space| space.maybe_my_role.is_some() || has_channel_in(&conversations, space.id))
      .map(|space| ListedSpace {
        id: space.id,
        name: space.name,
        maybe_image_version: space.maybe_image.map(|image| image.version),
      })
      .collect();
    let space_ids: BTreeSet<SpaceId> = spaces.iter().map(|space| space.id).collect();
    let remaining = conversations
      .into_iter()
      .filter(|conversation| {
        conversation
          .maybe_space_id
          .is_some_and(|space| space_ids.contains(&space))
          || is_direct(conversation)
      })
      .collect();
    let mut users = BTreeMap::new();
    if let Some(me) = self.summary_of(self.me_id()) {
      users.insert(me.id, me);
    }
    ListPass {
      spaces,
      remaining,
      listed: Vec::new(),
      users,
      preferences: self.mention_preferences(),
      account: self.account_data(),
      limit,
      slices: 0,
    }
  }

  /// Builds the next slice of `pass`'s entries: until the slice's time or count runs out, one entry
  /// at least. The counts it tallied are kept at its end.
  pub fn continue_list_pass(&self, pass: &mut ListPass) {
    let _step = crate::timing::step("chat_list_build");
    let started_ms = self.now_ms();
    pass.slices += 1;
    let memo = self.read_pass_memo();
    self.with_pass_memo(memo, || {
      let mut built = 0;
      while let Some(response) = pass.remaining.pop_front() {
        self.add_people_of(&response, &mut pass.users);
        let entry = self.listed_conversation(
          &response,
          &pass.users,
          &pass.preferences,
          &pass.account,
          EntryDetail::List,
        );
        pass.listed.push(entry);
        built += 1;
        if built >= pass.limit.max_entries || is_over(started_ms, self.now_ms(), pass.limit.max_ms) {
          break;
        }
      }
    });
    self.flush_entry_tallies();
  }

  /// Reads the people a direct message's entry names (participants and members) that `users`
  /// doesn't hold yet. A channel's entry names nobody.
  fn add_people_of(&self, response: &ConversationResponse, users: &mut BTreeMap<UserId, UserSummary>) {
    if !is_direct(response) {
      return;
    }
    let wanted: BTreeSet<UserId> = response
      .participants
      .iter()
      .map(|participant| participant.id)
      .chain(self.members(response.id))
      .filter(|user| !users.contains_key(user))
      .collect();
    for user in wanted {
      if let Some(summary) = self.summary_of(user) {
        users.insert(user, summary);
      }
    }
  }
}

fn is_direct(conversation: &ConversationResponse) -> bool {
  matches!(
    conversation.kind,
    ConversationKindProto::Dm | ConversationKindProto::Group
  )
}

/// Whether a slice that started at `started_ms` ran its time. A clock that went back ends it too.
fn is_over(started_ms: TimestampMs, now_ms: TimestampMs, max_ms: i64) -> bool {
  let elapsed = now_ms.saturating_sub(started_ms);
  elapsed >= max_ms || elapsed < 0
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_slice_ends_when_its_time_ran_out_or_the_clock_went_back() {
    assert!(!is_over(1_000, 1_039, 40));
    assert!(is_over(1_000, 1_040, 40));
    assert!(is_over(1_000, 999, 40));
    assert!(!is_over(1_000, i64::MAX, i64::MAX));
  }
}
