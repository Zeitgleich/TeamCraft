//! Anonymous polls (issue #2121, e2ee.md §4.12.1): the poll inside its message with the key its
//! ballots are sealed with, and the ballot box as this device read and counted it. Votes of an
//! anonymous poll never come from messages: members hand sealed ballots to the server's box, which
//! gives them out without their senders once the poll is closed.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use teamcraft_crypto::poll_ballot::{BallotKey, BallotScope, MAX_BALLOT_OPTIONS, PollBallotError};
use teamcraft_proto::{
  BallotBoxResponse, ClientMessageId, ConversationId, Id, PollResults, PollSpec, PollVoters,
};

use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, MessageContent, StoredMessage, Table};

/// An anonymous poll inside its message (e2ee.md §4.12.1): the poll, with `voters: anonymous` and
/// `results: after_close`, and the key every member seals and opens its ballots with.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnonymousPoll {
  #[serde(flatten)]
  pub spec: PollSpec,
  pub ballot_key: BallotKeyText,
}

/// A poll's ballot key as it travels inside the message (base64url). Never printed.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BallotKeyText(String);

/// An anonymous poll's ballot box as this device read it last, counted (in
/// [`Table::BallotBoxes`]). Votes come from the server's box, never from messages.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BallotBoxRecord {
  /// The poll's sender opened the box; until then nobody can vote.
  #[serde(default)]
  pub is_open: bool,
  #[serde(default)]
  pub ballot_count: u32,
  #[serde(default)]
  pub is_closed: bool,
  /// The user's own choices: from their ballot while open, or what this device last sent.
  #[serde(rename = "my_choices", default, skip_serializing_if = "Vec::is_empty")]
  pub my_choices: Vec<u32>,
  /// Per option, once closed and counted.
  #[serde(rename = "counts", default, skip_serializing_if = "Option::is_none")]
  pub maybe_counts: Option<Vec<u32>>,
  /// Ballots that counted (once closed): the voters.
  #[serde(default)]
  pub counted: u32,
  /// Ballots that didn't open, chose what the poll doesn't take, or repeated another's bytes.
  #[serde(default)]
  pub skipped: u32,
}

impl AnonymousPoll {
  /// A new anonymous poll of `spec` with a fresh ballot key: voters anonymous, counts after
  /// closing whatever `spec` said.
  pub fn new(spec: PollSpec, rng: &mut impl rand_core::CryptoRngCore) -> Self {
    let key = BallotKey::generate(rng);
    Self {
      spec: PollSpec {
        voters: PollVoters::Anonymous,
        results: PollResults::AfterClose,
        ..spec
      },
      ballot_key: BallotKeyText(key.to_text().as_str().to_owned()),
    }
  }

  /// The poll, if this device counts it: within the receivers' bounds, anonymous, counting after
  /// closing, with options a ballot can name and a readable key.
  pub(crate) fn counted_spec(&self) -> Option<&PollSpec> {
    let spec = &self.spec;
    let is_counted = spec.is_receivable()
      && spec.voters == PollVoters::Anonymous
      && spec.results == PollResults::AfterClose
      && spec.options.len() <= MAX_BALLOT_OPTIONS
      && self.key().is_ok();
    is_counted.then_some(spec)
  }

  pub fn key(&self) -> Result<BallotKey, PollBallotError> {
    BallotKey::from_text(&self.ballot_key.0)
  }
}

impl std::fmt::Debug for BallotKeyText {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str("BallotKeyText([redacted])")
  }
}

/// The scope a ballot of `poll` is sealed to.
pub fn ballot_scope(poll: &StoredMessage) -> BallotScope {
  BallotScope {
    conversation_id: poll.conversation_id.snowflake().get(),
    poll_message_id: *poll.client_message_id.as_bytes(),
  }
}

/// The record key of `poll`'s ballot box.
pub fn ballot_box_key(conversation: ConversationId, poll: ClientMessageId) -> String {
  format!("{conversation}/{poll}")
}

/// What this device last read of `poll`'s ballot box, if anything.
pub fn stored_ballot_box(
  store: &impl LocalStore,
  conversation: ConversationId,
  poll: ClientMessageId,
) -> Result<Option<BallotBoxRecord>, CoreError> {
  records::get_json(store, Table::BallotBoxes, &ballot_box_key(conversation, poll))
}

pub fn store_ballot_box(
  store: &impl LocalStore,
  conversation: ConversationId,
  poll: ClientMessageId,
  record: &BallotBoxRecord,
) -> Result<(), CoreError> {
  records::put_json(
    store,
    Table::BallotBoxes,
    &ballot_box_key(conversation, poll),
    record,
  )
}

/// The box as the server sent it, counted with the poll's key. `previous` keeps the user's own
/// choices once the server forgot whose ballot is whose.
pub fn counted_ballot_box(
  poll: &StoredMessage,
  anonymous: &AnonymousPoll,
  response: &BallotBoxResponse,
  maybe_previous: Option<&BallotBoxRecord>,
) -> BallotBoxRecord {
  let Ok(key) = anonymous.key() else {
    return BallotBoxRecord::default();
  };
  let scope = ballot_scope(poll);
  let spec = &anonymous.spec;
  let my_choices = match &response.maybe_mine {
    Some(mine) => key
      .open(scope, mine.as_ref())
      .ok()
      .filter(|choices| spec.choice.allows(choices, spec.options.len()))
      .unwrap_or_default(),
    None if response.is_closed => maybe_previous
      .map(|previous| previous.my_choices.clone())
      .unwrap_or_default(),
    None => Vec::new(),
  };
  let mut record = BallotBoxRecord {
    is_open: true,
    ballot_count: response.ballot_count,
    is_closed: response.is_closed,
    my_choices,
    ..BallotBoxRecord::default()
  };
  if response.is_closed {
    let (counts, counted, skipped) = count_ballots(&key, scope, spec, &response.ballots);
    record.maybe_counts = Some(counts);
    record.counted = counted;
    record.skipped = skipped;
  }
  record
}

/// Counts `ballots` of the poll `spec`: each that opens with `key` for `scope` and holds a vote the
/// poll takes counts once; byte-identical ballots count once. Returns the counts per option, the
/// ballots counted and those skipped.
pub fn count_ballots(
  key: &BallotKey,
  scope: BallotScope,
  spec: &PollSpec,
  ballots: &[teamcraft_proto::Base64Bytes],
) -> (Vec<u32>, u32, u32) {
  let mut counts = vec![0u32; spec.options.len()];
  let mut seen: BTreeSet<&[u8]> = BTreeSet::new();
  let (mut counted, mut skipped) = (0u32, 0u32);
  for ballot in ballots {
    let bytes = ballot.as_ref();
    let maybe_choices = seen
      .insert(bytes)
      .then(|| key.open(scope, bytes).ok())
      .flatten()
      .filter(|choices| !choices.is_empty() && spec.choice.allows(choices, spec.options.len()));
    let Some(choices) = maybe_choices else {
      skipped = skipped.saturating_add(1);
      continue;
    };
    counted = counted.saturating_add(1);
    for choice in choices {
      // Invariant: `allows` keeps only positions below the option count.
      if let Some(count) = usize::try_from(choice).ok().and_then(|at| counts.get_mut(at)) {
        *count = count.saturating_add(1);
      }
    }
  }
  (counts, counted, skipped)
}

/// The anonymous poll `message` carries, if it is one this device counts (see `polls::poll_of`).
pub fn anonymous_poll_of(message: &StoredMessage) -> Option<&AnonymousPoll> {
  if message.maybe_unreadable.is_some() {
    return None;
  }
  match &message.content {
    MessageContent::Text { meta, .. } if meta.maybe_poll.is_none() => meta
      .maybe_anonymous_poll
      .as_deref()
      .filter(|anonymous| anonymous.counted_spec().is_some()),
    _ => None,
  }
}

#[cfg(test)]
mod tests {
  use rand_core::OsRng;
  use teamcraft_proto::{Base64Bytes, PollChoice, Snowflake, UserId};

  use super::*;
  use crate::store::MessageMeta;

  mod counting {
    use super::*;

    #[test]
    fn each_valid_ballot_counts_once() {
      let (poll, anonymous) = anonymous_poll(PollChoice::Multiple);
      let key = anonymous.key().unwrap();
      let scope = ballot_scope(&poll);
      let first = seal(&key, scope, &[0, 2]);
      let ballots = [first.clone(), seal(&key, scope, &[2]), first];
      let (counts, counted, skipped) = count_ballots(&key, scope, &anonymous.spec, &ballots);
      assert_eq!(counts, vec![1, 0, 2]);
      assert_eq!((counted, skipped), (2, 1), "a byte-identical copy counts once");
    }

    #[test]
    fn ballots_the_poll_does_not_take_are_skipped() {
      let (poll, anonymous) = anonymous_poll(PollChoice::Single);
      let key = anonymous.key().unwrap();
      let scope = ballot_scope(&poll);
      let ballots = [
        seal(&key, scope, &[0, 1]),
        seal(&key, scope, &[5]),
        seal(&key, scope, &[]),
        seal(&key, scope, &[1]),
      ];
      let (counts, counted, skipped) = count_ballots(&key, scope, &anonymous.spec, &ballots);
      assert_eq!(counts, vec![0, 1, 0]);
      assert_eq!((counted, skipped), (1, 3));
    }

    #[test]
    fn ballots_of_another_poll_or_key_and_tampered_ones_are_skipped() {
      let (poll, anonymous) = anonymous_poll(PollChoice::Single);
      let key = anonymous.key().unwrap();
      let scope = ballot_scope(&poll);
      let other_scope = BallotScope {
        poll_message_id: [9; 16],
        ..scope
      };
      let other_key = BallotKey::generate(&mut OsRng);
      let mut tampered = seal(&key, scope, &[0]).as_ref().to_vec();
      tampered[1] ^= 0x80;
      let ballots = [
        seal(&key, other_scope, &[0]),
        seal(&other_key, scope, &[0]),
        Base64Bytes::from(tampered),
        Base64Bytes::from(Vec::new()),
      ];
      let (counts, counted, skipped) = count_ballots(&key, scope, &anonymous.spec, &ballots);
      assert_eq!(counts, vec![0, 0, 0]);
      assert_eq!((counted, skipped), (0, 4));
    }

    #[test]
    fn the_own_choice_stays_once_the_server_forgot_it() {
      let (poll, anonymous) = anonymous_poll(PollChoice::Single);
      let key = anonymous.key().unwrap();
      let mine = seal(&key, ballot_scope(&poll), &[2]);
      let open = BallotBoxResponse {
        ballot_count: 1,
        is_closed: false,
        maybe_closes_at_ms: None,
        maybe_mine: Some(mine.clone()),
        ballots: Vec::new(),
      };
      let before = counted_ballot_box(&poll, &anonymous, &open, None);
      assert_eq!(before.my_choices, vec![2]);
      assert_eq!(before.maybe_counts, None);
      let closed = BallotBoxResponse {
        is_closed: true,
        maybe_mine: None,
        ballots: vec![mine],
        ..open
      };
      let after = counted_ballot_box(&poll, &anonymous, &closed, Some(&before));
      assert_eq!(after.my_choices, vec![2]);
      assert_eq!(after.maybe_counts, Some(vec![0, 0, 1]));
      assert_eq!(after.counted, 1);
    }
  }

  mod recognizing {
    use super::*;

    #[test]
    fn an_anonymous_poll_is_one_only_in_its_own_field_with_a_key() {
      let (poll, _) = anonymous_poll(PollChoice::Single);
      assert!(anonymous_poll_of(&poll).is_some());
      let mut broken = poll.clone();
      if let MessageContent::Text { meta, .. } = &mut broken.content
        && let Some(anonymous) = meta.maybe_anonymous_poll.as_mut()
      {
        anonymous.ballot_key = BallotKeyText("short".into());
      }
      assert!(anonymous_poll_of(&broken).is_none(), "no readable key, no poll");
      let mut shown_results = poll.clone();
      if let MessageContent::Text { meta, .. } = &mut shown_results.content
        && let Some(anonymous) = meta.maybe_anonymous_poll.as_mut()
      {
        anonymous.spec.results = PollResults::Always;
      }
      assert!(
        anonymous_poll_of(&shown_results).is_none(),
        "counts before closing aren't anonymous"
      );
    }

    #[test]
    fn the_key_never_shows_in_debug_output() {
      let (_, anonymous) = anonymous_poll(PollChoice::Single);
      let printed = format!("{anonymous:?}");
      assert!(printed.contains("[redacted]"));
      assert!(!printed.contains(&anonymous.ballot_key.0));
    }
  }

  fn anonymous_poll(choice: PollChoice) -> (StoredMessage, AnonymousPoll) {
    let spec = PollSpec {
      question: "Lunch?".into(),
      options: vec!["Pizza".into(), "Salad".into(), "Soup".into()],
      choice,
      voters: PollVoters::Shown,
      maybe_closes_at_ms: None,
      results: PollResults::Always,
    };
    let anonymous = AnonymousPoll::new(spec, &mut OsRng);
    assert_eq!(anonymous.spec.voters, PollVoters::Anonymous);
    assert_eq!(anonymous.spec.results, PollResults::AfterClose);
    let mut content = MessageContent::text("Poll: Lunch?", Vec::new());
    if let MessageContent::Text { meta, .. } = &mut content {
      *meta = MessageMeta {
        maybe_anonymous_poll: Some(Box::new(anonymous.clone())),
        ..MessageMeta::default()
      };
    }
    let message = StoredMessage {
      conversation_id: ConversationId::from_snowflake(Snowflake::new(10).unwrap()),
      client_message_id: ClientMessageId::from_bytes([3; 16]),
      maybe_server_id: Some(teamcraft_proto::MessageId::from_snowflake(
        Snowflake::new(50).unwrap(),
      )),
      sender_user_id: UserId::from_snowflake(Snowflake::new(1).unwrap()),
      maybe_thread_id: None,
      sent_at_ms: 50,
      content,
      is_edited: false,
      edits: Vec::new(),
      maybe_edit_id: None,
      is_pinned: false,
      maybe_verified_commit: None,
      maybe_server_change: None,
      maybe_unreadable: None,
    };
    (message, anonymous)
  }

  fn seal(key: &BallotKey, scope: BallotScope, choices: &[u32]) -> Base64Bytes {
    Base64Bytes::from(key.seal(scope, choices, &mut OsRng).unwrap())
  }
}
