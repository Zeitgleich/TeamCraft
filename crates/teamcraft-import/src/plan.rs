//! What the import does with each Synology conversation (docs/import-synology.md §1, admin.md
//! §3.8), decided from the export, the mappings and the accounts alone. Pure, so the dry run and
//! the run decide the same.

use std::collections::{BTreeSet, HashMap};

use teamcraft_proto::UserId;

use crate::export::{
  CHANNEL_CHATBOT, CHANNEL_DEFAULT, CHANNEL_DM, CHANNEL_GROUP, CHANNEL_MY_SPACE, Channel, Export, SourceId,
};
use crate::mapping::{ChannelTarget, Mapping};

/// One conversation the import fills.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
  pub source: SourceId,
  /// The Synology name, for the report.
  pub title: String,
  pub target: Target,
  /// Posts are imported (else only the conversation and its members).
  pub has_history: bool,
  pub post_count: u64,
  /// Mapped members in the conversation (channels; a DM's are its participants).
  pub members: Vec<UserId>,
  /// Mapped members, the most active first (the creator first among equals): the first who has
  /// set up the app becomes the new channel's admin before the import leaves.
  pub admin_candidates: Vec<UserId>,
  pub is_archived: bool,
}

impl Item {
  /// The key of the item in the progress database.
  pub fn key(&self) -> String {
    format!("synology:{}", self.source)
  }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
  NewChannel {
    space: String,
    name: String,
    is_public: bool,
    maybe_topic: Option<String>,
  },
  /// A channel that exists in TeamCraft, by space and name: the history goes before its messages.
  ExistingChannel {
    space: String,
    name: String,
  },
  Dm {
    participants: [UserId; 2],
  },
  Notes {
    user: UserId,
  },
}

/// A conversation left out, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skip {
  pub source: SourceId,
  pub title: String,
  pub reason: SkipReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
  /// The channel has no row in the channel mapping.
  NotInMapping,
  /// The mapping says not to import it.
  Excluded,
  NoPosts,
  /// A conversation with a Synology chatbot.
  Chatbot,
  /// A DM with somebody the user mapping doesn't name: like a DM with a deleted account.
  UnmappedParticipant,
  /// The owner of a "My space" has no account.
  UnmappedOwner,
  /// `notes` in the mapping, but the channel doesn't have exactly one mapped member.
  NotesWithoutOneMember,
  /// A channel type this import doesn't know.
  UnknownType,
}

impl SkipReason {
  pub fn describe(self) -> &'static str {
    match self {
      Self::NotInMapping => "not in the channel mapping",
      Self::Excluded => "excluded in the channel mapping",
      Self::NoPosts => "no posts",
      Self::Chatbot => "a chatbot conversation",
      Self::UnmappedParticipant => "a participant has no account",
      Self::UnmappedOwner => "its owner has no account",
      Self::NotesWithoutOneMember => "notes need exactly one mapped member",
      Self::UnknownType => "an unknown channel type",
    }
  }
}

/// What the import does.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
  pub items: Vec<Item>,
  pub skips: Vec<Skip>,
}

/// Builds the plan. `accounts` are the mapped users that exist (Synology ID → account).
pub fn plan(export: &Export, mapping: &Mapping, accounts: &HashMap<SourceId, UserId>) -> Plan {
  let mut plan = Plan::default();
  for channel in export.channels.values() {
    let title = title_of(channel);
    let skip = |reason| Skip {
      source: channel.id,
      title: title.clone(),
      reason,
    };
    let post_count = export.post_count(channel.id);
    let mapped: Vec<UserId> = channel
      .members()
      .iter()
      .filter_map(|member| accounts.get(member).copied())
      .collect::<BTreeSet<_>>()
      .into_iter()
      .collect();
    let admin_candidates = by_activity(export, channel, accounts);
    let decided = match channel.kind {
      CHANNEL_CHATBOT => Err(SkipReason::Chatbot),
      CHANNEL_DM => dm_target(channel, accounts),
      CHANNEL_MY_SPACE => channel
        .members()
        .first()
        .or(channel.creator_id.as_ref())
        .and_then(|owner| accounts.get(owner))
        .map(|user| (Target::Notes { user: *user }, true))
        .ok_or(SkipReason::UnmappedOwner),
      CHANNEL_DEFAULT | CHANNEL_GROUP => channel_target(channel, mapping, &mapped),
      _ => Err(SkipReason::UnknownType),
    };
    match decided {
      Err(reason) => plan.skips.push(skip(reason)),
      Ok(_) if post_count == 0 => plan.skips.push(skip(SkipReason::NoPosts)),
      Ok((target, has_history)) => {
        let members = match &target {
          Target::Dm { participants } => participants.to_vec(),
          Target::Notes { user } => vec![*user],
          _ => mapped,
        };
        plan.items.push(Item {
          source: channel.id,
          title,
          target,
          has_history,
          post_count,
          members,
          admin_candidates,
          is_archived: channel.is_archived(),
        });
      },
    }
  }
  plan
}

fn dm_target(channel: &Channel, accounts: &HashMap<SourceId, UserId>) -> Result<(Target, bool), SkipReason> {
  let people: BTreeSet<SourceId> = channel.members().iter().copied().collect();
  let mapped: Vec<UserId> = people
    .iter()
    .filter_map(|person| accounts.get(person).copied())
    .collect();
  match (people.len(), mapped.as_slice()) {
    (2, [first, second]) if first != second => Ok((
      Target::Dm {
        participants: [*first, *second],
      },
      true,
    )),
    // A DM with oneself.
    (1, [user]) => Ok((Target::Notes { user: *user }, true)),
    _ => Err(SkipReason::UnmappedParticipant),
  }
}

fn channel_target(
  channel: &Channel,
  mapping: &Mapping,
  mapped: &[UserId],
) -> Result<(Target, bool), SkipReason> {
  let row = mapping
    .channels
    .get(&channel.id)
    .ok_or(SkipReason::NotInMapping)?;
  if !row.is_included {
    return Err(SkipReason::Excluded);
  }
  let target = match &row.target {
    ChannelTarget::Notes => match mapped {
      [user] => Target::Notes { user: *user },
      _ => return Err(SkipReason::NotesWithoutOneMember),
    },
    ChannelTarget::Existing { space, channel } => Target::ExistingChannel {
      space: space.clone(),
      name: channel.clone(),
    },
    ChannelTarget::New => Target::NewChannel {
      space: row.space.clone(),
      name: match &row.maybe_teamcraft_name {
        Some(teamcraft_name) => teamcraft_name.clone(),
        None if row.name.is_empty() => title_of(channel),
        None => row.name.clone(),
      },
      is_public: row.is_public,
      maybe_topic: channel
        .purpose
        .clone()
        .filter(|purpose| !purpose.trim().is_empty()),
    },
  };
  Ok((target, row.has_history))
}

/// The mapped members, the most posts in the channel first, the creator first among equals.
fn by_activity(export: &Export, channel: &Channel, accounts: &HashMap<SourceId, UserId>) -> Vec<UserId> {
  let counts = export.authors(channel.id);
  let mut ranked: Vec<(u64, bool, UserId)> = channel
    .members()
    .iter()
    .filter_map(|member| {
      accounts.get(member).map(|user| {
        (
          counts.get(member).copied().unwrap_or(0),
          channel.creator_id == Some(*member),
          *user,
        )
      })
    })
    .collect();
  ranked.sort_by(|left, right| {
    right
      .0
      .cmp(&left.0)
      .then(right.1.cmp(&left.1))
      .then(left.2.cmp(&right.2))
  });
  ranked.dedup_by_key(|entry| entry.2);
  ranked.into_iter().map(|(_, _, user)| user).collect()
}

/// The channel's name in Synology.
pub fn title_of(channel: &Channel) -> String {
  [&channel.display_name, &channel.name]
    .into_iter()
    .flatten()
    .find(|name| !name.trim().is_empty())
    .cloned()
    .unwrap_or_else(|| format!("channel {}", channel.id))
}
