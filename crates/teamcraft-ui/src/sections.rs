//! Pure grouping of conversations into sidebar sections: the channels of a space, and the
//! Home (DMs and group DMs live outside spaces).

use std::collections::{HashMap, HashSet};

use teamcraft_client_api::activity::{ActivityItem, ActivityKind};

use crate::model::{Conversation, ConversationId, ConversationKind, SidebarCategory, SpaceId, TimestampMs};
use crate::prefs::{DmSort, SidebarSort};

// The rule lives in core, shared with the mobile apps (#897).
pub use teamcraft_core::row_badge::{RowBadge, RowBadgeTone, row_badge};

/// Sidebar sections of a space in display order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SectionKind {
  Unreads,
  Favorites,
  Channels,
  Integrations,
  /// The DMs and group DMs with people of the space (#701), by recent activity. Each is still the
  /// one conversation Chats lists.
  DirectMessages,
  /// Only for guests, who have no channel browser to find them in.
  Archived,
}

impl SectionKind {
  pub const ALL: [SectionKind; 6] = [
    Self::Unreads,
    Self::Favorites,
    Self::Channels,
    Self::Integrations,
    Self::DirectMessages,
    Self::Archived,
  ];

  pub fn from_slug(slug: &str) -> Option<Self> {
    Self::ALL.into_iter().find(|kind| kind.slug() == slug)
  }

  pub fn slug(self) -> &'static str {
    match self {
      Self::Unreads => "unreads",
      Self::Favorites => "favorites",
      Self::Channels => "channels",
      Self::Integrations => "integrations",
      Self::DirectMessages => "direct-messages",
      Self::Archived => "archived",
    }
  }

  /// i18n key of the section title.
  pub fn title_key(self) -> &'static str {
    match self {
      Self::Unreads => "sidebar-unreads",
      Self::Favorites => "sidebar-favorites",
      Self::Channels => "sidebar-channels",
      Self::Integrations => "sidebar-integrations",
      Self::DirectMessages => "sidebar-direct-messages",
      Self::Archived => "sidebar-archived",
    }
  }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Section {
  pub kind: SectionKind,
  pub conversations: Vec<Conversation>,
}

/// How the sidebar of a space groups and orders its channels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SidebarOptions {
  pub show_unreads_group: bool,
  pub sort: SidebarSort,
  /// The conversation that was unread when the user opened it: it stays in the "Unreads" section
  /// (shown as read) until they open another one, so the list doesn't shift under the pointer.
  pub maybe_held_open: Option<ConversationId>,
  /// Lists archived channels in a section of their own, for guests. Everybody else finds them in
  /// the channel browser.
  pub lists_archived: bool,
}

impl SidebarOptions {
  pub fn plain(show_unreads_group: bool) -> Self {
    Self {
      show_unreads_group,
      sort: SidebarSort::Default,
      maybe_held_open: None,
      lists_archived: false,
    }
  }
}

/// Groups the channels of a space: unread ones (optional), favorites, then the remaining by
/// category. Every conversation appears exactly once; empty sections are omitted. Direct messages
/// passed in (those with people of the space, #701) go into their own section in the order given,
/// whatever their unread count or star: they are Chats' conversations, shown here too, and a
/// space's list shouldn't move them around.
pub fn build_sections(conversations: &[Conversation], options: &SidebarOptions) -> Vec<Section> {
  let mut sections: Vec<Section> = SectionKind::ALL
    .into_iter()
    .map(|kind| Section {
      kind,
      conversations: Vec::new(),
    })
    .collect();

  for conversation in conversations
    .iter()
    // Archived channels leave the list; Browse finds them, and the open one stays while it is open.
    .filter(|conversation| {
      !conversation.is_archived || options.lists_archived || options.maybe_held_open == Some(conversation.id)
    })
  {
    let is_unread = conversation.unread_count > 0 && !conversation.is_muted;
    let is_held_open = options.maybe_held_open == Some(conversation.id);
    let kind = if conversation.kind.is_direct_like() {
      SectionKind::DirectMessages
    } else if conversation.is_archived && options.lists_archived {
      SectionKind::Archived
    } else if options.show_unreads_group && (is_unread || is_held_open) {
      SectionKind::Unreads
    } else if conversation.is_favorite {
      SectionKind::Favorites
    } else {
      match conversation.category {
        SidebarCategory::Favorites => SectionKind::Favorites,
        SidebarCategory::Integrations => SectionKind::Integrations,
        SidebarCategory::Channels | SidebarCategory::DirectMessages => SectionKind::Channels,
      }
    };
    if let Some(section) = sections.iter_mut().find(|section| section.kind == kind) {
      section.conversations.push(conversation.clone());
    }
  }
  for section in &mut sections {
    let keeps_order = matches!(section.kind, SectionKind::Unreads | SectionKind::DirectMessages);
    if options.sort == SidebarSort::Alphabetical && !keeps_order {
      section
        .conversations
        .sort_by_key(|conversation| conversation.name.to_lowercase());
    }
  }
  sections.retain(|section| !section.conversations.is_empty());
  sections
}

/// Whether Chats lists the conversation: DMs and group DMs, and channels of a space the user starred
/// (#1152). Any other channel belongs to its space only.
pub fn is_listed_in_chats(conversation: &Conversation) -> bool {
  conversation.kind.is_direct_like()
    || (conversation.is_favorite && conversation.maybe_space.is_some() && !conversation.is_archived)
}

/// The sections of Home, in display order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DirectSectionKind {
  /// The user's notes to themself, pinned above everything (no heading).
  Notes,
  /// Only in the list layout, with the setting for the section on ([`build_list`]).
  Unreads,
  Favorites,
  Conversations,
  /// The list layout's conversations that are in no section above ([`build_list`]). Grouped by
  /// space, the channels of a space the list doesn't know.
  Everything,
  /// The list layout grouped by space (#1345): the channels of one space that are in no section
  /// above, under the space's name.
  Space(SpaceId),
  /// DMs whose other people are all deactivated (issue #698): after the active ones, never cut
  /// off, so their history stays a click away.
  Deactivated,
}

impl DirectSectionKind {
  /// i18n key of the section title.
  pub fn title_key(self) -> &'static str {
    match self {
      Self::Notes => "notes-title",
      Self::Unreads => "sidebar-unreads",
      Self::Favorites => "sidebar-favorites",
      Self::Conversations => "sidebar-direct-messages",
      Self::Everything => "sidebar-conversations",
      // The heading is the space's own name; this one stands in while the space is unknown.
      Self::Space(_) => "sidebar-channels",
      Self::Deactivated => "sidebar-deactivated-accounts",
    }
  }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DirectSection {
  pub kind: DirectSectionKind,
  pub conversations: Vec<Conversation>,
}

/// The filter chips above the Home list (Element: Unread, People, Groups). One at a time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum HomeFilter {
  #[default]
  All,
  /// Conversations with unread messages (muted ones only with mentions).
  Unread,
  /// One-to-one DMs.
  People,
  /// Group DMs.
  Groups,
  /// Channels of spaces; a chip of the list layout only.
  Channels,
}

impl HomeFilter {
  /// The chips, in display order ("All" is no chip selected).
  pub const CHIPS: [HomeFilter; 3] = [Self::Unread, Self::People, Self::Groups];
  /// The chips of the list layout, where channels are in the list too.
  pub const LIST_CHIPS: [HomeFilter; 4] = [Self::Unread, Self::People, Self::Groups, Self::Channels];

  /// i18n key of the chip.
  pub fn label_key(self) -> &'static str {
    match self {
      Self::All => "home-filter-all",
      Self::Unread => "home-filter-unread",
      Self::People => "home-filter-people",
      Self::Groups => "home-filter-groups",
      Self::Channels => "home-filter-channels",
    }
  }

  pub fn matches(self, conversation: &Conversation) -> bool {
    match self {
      Self::All => true,
      Self::Unread => {
        conversation.mention_count > 0 || (conversation.unread_count > 0 && !conversation.is_muted)
      },
      Self::People => conversation.kind == ConversationKind::Direct,
      Self::Groups => conversation.kind == ConversationKind::Group,
      Self::Channels => !conversation.kind.is_direct_like(),
    }
  }
}

/// The list of Home.
#[derive(Clone, Debug, PartialEq)]
pub struct DirectList {
  pub sections: Vec<DirectSection>,
  /// Conversations cut off by the display limit ("show more").
  pub hidden_count: usize,
}

/// Lists DMs and group DMs (each with the time of its newest message), and the favorite channels
/// of spaces among them (#1152): favorites, then the rest, both in `sort` order, and last the DMs of deactivated accounts (issue #698). Only those `chip`
/// lets through and whose names contain `filter` (any case) are kept; while filtering the limit
/// doesn't apply. `limit` 0 shows all; unread conversations stay visible beyond it.
pub fn build_direct(
  conversations: Vec<(Conversation, Option<TimestampMs>)>,
  sort: DmSort,
  limit: usize,
  filter: &str,
  chip: HomeFilter,
) -> DirectList {
  let list = matching_in_order(
    conversations
      .into_iter()
      .filter(|(conversation, _)| is_listed_in_chats(conversation))
      .collect(),
    sort,
    filter,
    chip,
  );
  let (notes, list): (Vec<_>, Vec<_>) = list
    .into_iter()
    .partition(|conversation| conversation.is_notes_to_self);
  // A deactivated account's DM leaves the active list, favorite or not (issue #698).
  let (deactivated, list): (Vec<Conversation>, Vec<Conversation>) = list
    .into_iter()
    .partition(|conversation| conversation.has_only_deactivated_others);
  let (favorites, rest): (Vec<Conversation>, Vec<Conversation>) = list
    .into_iter()
    .partition(|conversation| conversation.is_favorite);
  let mut hidden_count = 0;
  let is_filtering = !filter.trim().is_empty() || chip != HomeFilter::All;
  let rest = if limit > 0 && !is_filtering && rest.len() > limit {
    let (keep, cut): (Vec<_>, Vec<_>) = rest
      .into_iter()
      .enumerate()
      .partition(|(index, conversation)| *index < limit || conversation.unread_count > 0);
    hidden_count = cut.len();
    keep.into_iter().map(|(_, conversation)| conversation).collect()
  } else {
    rest
  };
  let sections = [
    (DirectSectionKind::Notes, notes),
    (DirectSectionKind::Favorites, favorites),
    (DirectSectionKind::Conversations, rest),
    (DirectSectionKind::Deactivated, deactivated),
  ]
  .into_iter()
  .filter(|(_, conversations)| !conversations.is_empty())
  .map(|(kind, conversations)| DirectSection { kind, conversations })
  .collect();
  DirectList {
    sections,
    hidden_count,
  }
}

/// What [`build_list`] is to show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ListOptions<'a> {
  pub sort: DmSort,
  pub show_unreads_group: bool,
  /// Stays in "Unreads" while it is open, as in a space's sidebar ([`SidebarOptions`]).
  pub maybe_held_open: Option<ConversationId>,
  pub filter: &'a str,
  pub chip: HomeFilter,
  /// The most rows to hand out, 0 for all: the page the sidebar has come to. The list is cut here
  /// whatever the filters, so what gets rendered doesn't follow the size of the workspace.
  pub max_rows: usize,
  /// Grouped by space (#1345): the spaces in the order of their groups. `None` keeps direct
  /// messages and channels together.
  pub maybe_space_order: Option<&'a [SpaceId]>,
}

/// The one list of the list layout (#1151), for direct messages and the channels of all spaces: the
/// notes, unread ones when the setting wants them apart, favorites, everything else, and last the
/// DMs of deactivated accounts. Each section in `sort` order. Every conversation is one row.
///
/// Grouped by space (#1345, [`ListOptions::maybe_space_order`]), "everything else" becomes the
/// direct messages, then one section per space in that order, each holding the space's channels
/// that are in no section above. Unreads and favorites stay above the groups, across spaces, as in
/// the grouped sidebar of the spaces layout ([`build_grouped`]). A space without such channels has
/// no section.
pub fn build_list(
  conversations: Vec<(Conversation, Option<TimestampMs>)>,
  options: &ListOptions,
) -> DirectList {
  let list = matching_in_order(conversations, options.sort, options.filter, options.chip);
  let total = list.len();
  let mut kinds = vec![
    DirectSectionKind::Notes,
    DirectSectionKind::Unreads,
    DirectSectionKind::Favorites,
  ];
  if let Some(order) = options.maybe_space_order {
    kinds.push(DirectSectionKind::Conversations);
    kinds.extend(order.iter().map(|space| DirectSectionKind::Space(*space)));
  }
  kinds.extend([DirectSectionKind::Everything, DirectSectionKind::Deactivated]);
  // A space named twice in the order keeps its first place.
  let mut place: HashMap<DirectSectionKind, usize> = HashMap::with_capacity(kinds.len());
  let mut sections: Vec<DirectSection> = Vec::with_capacity(kinds.len());
  for kind in kinds {
    if let std::collections::hash_map::Entry::Vacant(slot) = place.entry(kind) {
      slot.insert(sections.len());
      sections.push(DirectSection {
        kind,
        conversations: Vec::new(),
      });
    }
  }
  for conversation in list {
    let is_waiting =
      HomeFilter::Unread.matches(&conversation) || options.maybe_held_open == Some(conversation.id);
    let kind = if conversation.is_notes_to_self {
      DirectSectionKind::Notes
    } else if conversation.has_only_deactivated_others {
      DirectSectionKind::Deactivated
    } else if options.show_unreads_group && is_waiting {
      DirectSectionKind::Unreads
    } else if conversation.is_favorite {
      DirectSectionKind::Favorites
    } else if options.maybe_space_order.is_none() {
      DirectSectionKind::Everything
    } else if conversation.kind.is_direct_like() {
      DirectSectionKind::Conversations
    } else {
      conversation
        .maybe_space
        .map(DirectSectionKind::Space)
        .filter(|kind| place.contains_key(kind))
        .unwrap_or(DirectSectionKind::Everything)
    };
    if let Some(index) = place.get(&kind) {
      sections[*index].conversations.push(conversation);
    }
  }
  let mut room = if options.max_rows == 0 {
    total
  } else {
    options.max_rows
  };
  for section in &mut sections {
    section.conversations.truncate(room);
    room -= section.conversations.len();
  }
  sections.retain(|section| !section.conversations.is_empty());
  DirectList {
    hidden_count: total
      - sections
        .iter()
        .map(|section| section.conversations.len())
        .sum::<usize>(),
    sections,
  }
}

/// One of each conversation, those `chip` lets through whose names contain `filter` (any case), in
/// `sort` order.
fn matching_in_order(
  conversations: Vec<(Conversation, Option<TimestampMs>)>,
  sort: DmSort,
  filter: &str,
  chip: HomeFilter,
) -> Vec<Conversation> {
  let needle = filter.trim().to_lowercase();
  let mut seen = HashSet::new();
  let mut list: Vec<(Conversation, Option<TimestampMs>)> = conversations
    .into_iter()
    // Each conversation is one row, however often a caller hands it over (issue #643).
    .filter(|(conversation, _)| seen.insert(conversation.id))
    .filter(|(conversation, _)| chip.matches(conversation))
    .filter(|(conversation, _)| needle.is_empty() || conversation.name.to_lowercase().contains(&needle))
    .collect();
  match sort {
    DmSort::Recent => list.sort_by(|(a, a_at), (b, b_at)| {
      b_at
        .cmp(a_at)
        .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    }),
    DmSort::Alphabetical => list.sort_by_key(|(conversation, _)| conversation.name.to_lowercase()),
  }
  list.into_iter().map(|(conversation, _)| conversation).collect()
}

/// What a space's rail entry counts for its conversations (#955): the sum of the red numbers their
/// sidebar rows show ([`row_badge`]), so a muted conversation adds only its mentions. The mentions
/// are counted apart for the accessible name.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RailBadge {
  pub count: u32,
  pub mention_count: u32,
}

impl RailBadge {
  pub fn of(conversations: &[Conversation]) -> Self {
    conversations.iter().fold(Self::default(), |sum, conversation| {
      let red = row_badge(
        conversation.unread_count,
        conversation.mention_count,
        conversation.is_muted,
      )
      .filter(|badge| badge.tone == RowBadgeTone::Red)
      .map_or(0, |badge| badge.count);
      Self {
        count: sum.count.saturating_add(red),
        mention_count: sum.mention_count.saturating_add(conversation.mention_count),
      }
    })
  }
}

/// Whether an unread activity item is addressed to the user: a mention, or anything in a direct or
/// group conversation. `is_direct` tells whether a conversation is one.
pub fn has_urgent_activity(items: &[ActivityItem], is_direct: impl Fn(ConversationId) -> bool) -> bool {
  items
    .iter()
    .filter(|item| item.is_unread)
    .any(|item| item.kind == ActivityKind::Mention || is_direct(item.conversation))
}

/// The number on the app's badge (dock, taskbar, tray): mentions everywhere plus unread direct and
/// group messages, as people know it from other chat apps. Muted conversations count only mentions.
pub fn badge_count(conversations: &[Conversation]) -> u32 {
  conversations
    .iter()
    // No list row to clear it from.
    .filter(|conversation| !conversation.is_archived)
    .map(|conversation| {
      let is_direct = matches!(
        conversation.kind,
        ConversationKind::Direct | ConversationKind::Group
      );
      if is_direct && !conversation.is_muted {
        conversation.unread_count.max(conversation.mention_count)
      } else {
        conversation.mention_count
      }
    })
    .sum()
}

/// Puts the favorites of `list` into the user's own order (IDs in `order` first, in that order; the
/// rest after them as they were). Other conversations keep their places.
pub fn apply_favorite_order(list: &mut [Conversation], order: &[ConversationId]) {
  if order.is_empty() {
    return;
  }
  let slots: Vec<usize> = (0..list.len()).filter(|index| list[*index].is_favorite).collect();
  let mut favorites: Vec<Conversation> = slots.iter().map(|index| list[*index].clone()).collect();
  favorites.sort_by_key(|conversation| {
    order
      .iter()
      .position(|id| *id == conversation.id)
      .unwrap_or(usize::MAX)
  });
  for (slot, conversation) in slots.into_iter().zip(favorites) {
    list[slot] = conversation;
  }
}

/// The new favorite order after moving `moved` to position `to` of the favorites as displayed.
pub fn move_favorite(displayed: &[ConversationId], moved: ConversationId, to: usize) -> Vec<ConversationId> {
  let mut order: Vec<ConversationId> = displayed.iter().copied().filter(|id| *id != moved).collect();
  if displayed.contains(&moved) {
    order.insert(to.min(order.len()), moved);
  }
  order
}

/// The channels of one space in the grouped sidebar.
#[derive(Clone, Debug, PartialEq)]
pub struct SpaceGroup {
  pub space: SpaceId,
  pub name: String,
  /// Channels first, then integrations; unread and favorite ones are in [`GroupedSidebar::top`].
  pub conversations: Vec<Conversation>,
}

/// The "all spaces in one sidebar" layout: unread and favorite channels of every space on top, then
/// one group per space.
#[derive(Clone, Debug, PartialEq)]
pub struct GroupedSidebar {
  pub top: Vec<Section>,
  pub spaces: Vec<SpaceGroup>,
}

/// Splits [`build_sections`] by space. `spaces` gives the order and names of the groups; a channel
/// of a space not listed there goes into no group. Spaces without channels are omitted.
pub fn build_grouped(
  spaces: &[(SpaceId, String)],
  conversations: &[Conversation],
  options: &SidebarOptions,
) -> GroupedSidebar {
  let mut top = Vec::new();
  let mut channels: Vec<Conversation> = Vec::new();
  for section in build_sections(conversations, options) {
    match section.kind {
      SectionKind::Unreads | SectionKind::Favorites => top.push(section),
      // The grouped sidebar holds the channels of every space; DMs are in Chats.
      SectionKind::DirectMessages => {},
      SectionKind::Channels | SectionKind::Integrations | SectionKind::Archived => {
        channels.extend(section.conversations);
      },
    }
  }
  let spaces = spaces
    .iter()
    .map(|(space, name)| SpaceGroup {
      space: *space,
      name: name.clone(),
      conversations: channels
        .iter()
        .filter(|conversation| conversation.maybe_space == Some(*space))
        .cloned()
        .collect(),
    })
    .filter(|group| !group.conversations.is_empty())
    .collect();
  GroupedSidebar { top, spaces }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::UserId;

  #[test]
  fn badge_counts_mentions_and_unread_direct_messages() {
    let mut channel = conversation(1, SidebarCategory::Channels, 7, false);
    channel.mention_count = 2;
    let mut direct = conversation(2, SidebarCategory::DirectMessages, 3, false);
    direct.kind = ConversationKind::Direct;
    let mut muted_direct = conversation(3, SidebarCategory::DirectMessages, 4, false);
    muted_direct.kind = ConversationKind::Group;
    muted_direct.is_muted = true;
    muted_direct.mention_count = 1;
    assert_eq!(badge_count(&[channel, direct, muted_direct]), 2 + 3 + 1);
    assert_eq!(badge_count(&[]), 0);
  }

  mod urgent_activity {
    use teamcraft_client_api::activity::ActivityKind;

    use super::*;

    #[test]
    fn mentions_and_direct_messages_are_urgent_other_unread_is_not() {
      let reply = item(ActivityKind::ThreadReply, 1, true);
      let reaction = item(ActivityKind::Reaction { emoji: "x".into() }, 1, true);
      let mention = item(ActivityKind::Mention, 1, true);
      let direct_reply = item(ActivityKind::ThreadReply, 2, true);
      let read_mention = item(ActivityKind::Mention, 1, false);
      let is_direct = |id: ConversationId| id == crate::model::id_from_raw(2);
      assert!(!has_urgent_activity(&[reply.clone(), reaction], is_direct));
      assert!(has_urgent_activity(&[reply.clone(), mention], is_direct));
      assert!(has_urgent_activity(&[reply.clone(), direct_reply], is_direct));
      assert!(!has_urgent_activity(&[reply, read_mention], is_direct));
      assert!(!has_urgent_activity(&[], is_direct));
    }

    fn item(kind: ActivityKind, conversation: u64, is_unread: bool) -> ActivityItem {
      ActivityItem {
        id: "a".into(),
        kind,
        conversation: crate::model::id_from_raw(conversation),
        message: crate::model::id_from_raw(9),
        maybe_thread: None,
        actor: crate::model::id_from_raw(3),
        maybe_source_name: None,
        at: 0,
        excerpt: String::new(),
        maybe_read_through: None,
        is_unread,
      }
    }
  }

  mod favorite_order {
    use super::*;

    fn ids(list: &[Conversation]) -> Vec<u64> {
      list
        .iter()
        .map(|conversation| crate::model::raw_id(conversation.id))
        .collect()
    }

    fn sample() -> Vec<Conversation> {
      (1..=5)
        .map(|raw| conversation(raw, SidebarCategory::Channels, 0, raw % 2 == 1))
        .collect()
    }

    #[test]
    fn favorites_follow_the_order_and_others_keep_their_place() {
      // Favorites 1, 3, 5; 2 and 4 are not.
      let mut list = sample();
      apply_favorite_order(
        &mut list,
        &[crate::model::id_from_raw(5), crate::model::id_from_raw(1)],
      );
      assert_eq!(ids(&list), [5, 2, 1, 4, 3]);
    }

    #[test]
    fn an_empty_order_changes_nothing() {
      let mut list = sample();
      apply_favorite_order(&mut list, &[]);
      assert_eq!(ids(&list), [1, 2, 3, 4, 5]);
    }

    #[test]
    fn moving_a_favorite() {
      let shown: Vec<ConversationId> = [1, 3, 5].into_iter().map(crate::model::id_from_raw).collect();
      let moved = move_favorite(&shown, crate::model::id_from_raw(5), 0);
      assert_eq!(
        moved
          .iter()
          .map(|id| crate::model::raw_id(*id))
          .collect::<Vec<_>>(),
        [5, 1, 3]
      );
      let clamped = move_favorite(&shown, crate::model::id_from_raw(1), 99);
      assert_eq!(
        clamped
          .iter()
          .map(|id| crate::model::raw_id(*id))
          .collect::<Vec<_>>(),
        [3, 5, 1]
      );
      let unknown = move_favorite(&shown, crate::model::id_from_raw(9), 0);
      assert_eq!(unknown, shown);
    }
  }

  #[test]
  fn section_slugs_round_trip() {
    for kind in SectionKind::ALL {
      assert_eq!(SectionKind::from_slug(kind.slug()), Some(kind));
    }
    assert_eq!(SectionKind::from_slug("nope"), None);
  }
  use crate::model::{ConversationId, ConversationKind, id_from_raw};

  mod held_open {
    use super::*;

    #[test]
    fn the_conversation_opened_while_unread_stays_in_the_unreads_section_while_it_is_open() {
      // Open and already read: without the hold it would jump back to its place.
      let list = vec![
        conversation(1, SidebarCategory::Channels, 0, false),
        conversation(2, SidebarCategory::Channels, 4, false),
      ];
      let held = SidebarOptions {
        maybe_held_open: Some(id_from_raw::<ConversationId>(1)),
        ..SidebarOptions::plain(true)
      };

      let sections = build_sections(&list, &held);

      assert_eq!(sections[0].kind, SectionKind::Unreads);
      let unread_ids: Vec<ConversationId> = sections[0].conversations.iter().map(|c| c.id).collect();
      assert_eq!(
        unread_ids,
        [id_from_raw::<ConversationId>(1), id_from_raw::<ConversationId>(2)]
      );
    }

    #[test]
    fn without_the_section_nothing_is_held_and_the_list_keeps_its_places() {
      let list = vec![
        conversation(1, SidebarCategory::Channels, 0, false),
        conversation(2, SidebarCategory::Channels, 4, false),
      ];
      let held = SidebarOptions {
        maybe_held_open: Some(id_from_raw::<ConversationId>(1)),
        ..SidebarOptions::plain(false)
      };

      let sections = build_sections(&list, &held);

      assert_eq!(sections.len(), 1);
      assert_eq!(sections[0].kind, SectionKind::Channels);
      assert_eq!(sections[0].conversations.len(), 2);
    }

    #[test]
    fn the_unreads_section_is_off_by_default() {
      assert!(!crate::prefs::SidebarPrefs::default().shows_unreads_section);
      // Preferences stored before the setting existed read as off.
      let stored = crate::prefs::Preferences::from_json(r#"{"sidebar":{"sort":"default"}}"#);
      assert!(!stored.sidebar.shows_unreads_section);
    }
  }

  #[test]
  fn each_conversation_appears_once_and_direct_messages_in_their_own_section() {
    let list = vec![
      conversation(1, SidebarCategory::Channels, 3, false),
      conversation(2, SidebarCategory::Channels, 0, true),
      direct(3, 2, None).0,
      conversation(4, SidebarCategory::Integrations, 0, false),
    ];
    let sections = build_sections(&list, &SidebarOptions::plain(true));
    let kinds: Vec<SectionKind> = sections.iter().map(|section| section.kind).collect();
    assert_eq!(
      kinds,
      [
        SectionKind::Unreads,
        SectionKind::Favorites,
        SectionKind::Integrations,
        SectionKind::DirectMessages
      ]
    );
    assert_eq!(
      sections
        .iter()
        .map(|section| section.conversations.len())
        .sum::<usize>(),
      4
    );
  }

  /// #701: a space's DMs keep the order they come in (recent activity), unread, starred or sorted
  /// by name.
  #[test]
  fn direct_messages_keep_their_order() {
    let mut starred = direct(5, 0, None).0;
    starred.is_favorite = true;
    starred.name = "aaa".into();
    let list = vec![direct(7, 4, None).0, starred, direct(6, 0, None).0];
    let options = SidebarOptions {
      sort: SidebarSort::Alphabetical,
      ..SidebarOptions::plain(true)
    };
    let sections = build_sections(&list, &options);
    assert_eq!(sections.len(), 1);
    assert_eq!(sections[0].kind, SectionKind::DirectMessages);
    let ids: Vec<ConversationId> = sections[0].conversations.iter().map(|c| c.id).collect();
    assert_eq!(ids, [7, 5, 6].map(id_from_raw::<ConversationId>).to_vec());
  }

  #[test]
  fn channels_sort_by_name() {
    let mut list = vec![
      conversation(1, SidebarCategory::Channels, 0, false),
      conversation(2, SidebarCategory::Channels, 0, false),
    ];
    list[0].name = "zed".into();
    list[1].name = "Amy".into();
    let options = SidebarOptions {
      show_unreads_group: true,
      sort: SidebarSort::Alphabetical,
      maybe_held_open: None,
      lists_archived: false,
    };
    let names: Vec<String> = build_sections(&list, &options)[0]
      .conversations
      .iter()
      .map(|c| c.name.clone())
      .collect();
    assert_eq!(names, ["Amy", "zed"]);
  }

  /// #1152: the channels a user starred show under Favorites in Chats, next to the favorite DMs.
  mod archived_channels {
    use super::*;

    fn archived(id: u64) -> Conversation {
      let mut channel = conversation(id, SidebarCategory::Channels, 0, true);
      channel.maybe_space = Some(id_from_raw::<SpaceId>(7));
      channel.is_archived = true;
      channel
    }

    #[test]
    fn they_leave_the_lists_unless_they_are_open() {
      let all = [archived(1)];
      assert!(build_sections(&all, &SidebarOptions::plain(true)).is_empty());
      assert!(
        !is_listed_in_chats(&all[0]),
        "not under Chats either, even when starred"
      );
      let open = SidebarOptions {
        maybe_held_open: Some(all[0].id),
        ..SidebarOptions::plain(true)
      };
      assert_eq!(
        build_sections(&all, &open).len(),
        1,
        "the open one doesn't vanish under the reader"
      );
    }

    #[test]
    fn guests_find_them_in_a_section_of_their_own() {
      let all = [archived(1)];
      let guest = SidebarOptions {
        lists_archived: true,
        ..SidebarOptions::plain(true)
      };
      let sections = build_sections(&all, &guest);
      assert_eq!(sections.len(), 1);
      assert_eq!(sections[0].kind, SectionKind::Archived);
    }

    #[test]
    fn the_badge_counts_nothing_without_a_row() {
      let mut channel = archived(1);
      channel.mention_count = 3;
      assert_eq!(badge_count(&[channel]), 0);
    }
  }

  mod favorite_channels_in_chats {
    use super::*;

    fn starred(id: u64, space: u64) -> Conversation {
      let mut channel = conversation(id, SidebarCategory::Channels, 0, true);
      channel.maybe_space = Some(id_from_raw::<SpaceId>(space));
      channel
    }

    fn listed(entries: Vec<(Conversation, Option<TimestampMs>)>) -> DirectList {
      build_direct(entries, DmSort::Recent, 0, "", HomeFilter::All)
    }

    fn ids(list: &DirectList, kind: DirectSectionKind) -> Vec<u64> {
      list
        .sections
        .iter()
        .filter(|section| section.kind == kind)
        .flat_map(|section| section.conversations.iter().map(|c| crate::model::raw_id(c.id)))
        .collect()
    }

    #[test]
    fn a_starred_channel_sits_among_the_favorite_dms_by_recency() {
      let mut dm = direct(1, 0, Some(10));
      dm.0.is_favorite = true;
      let list = listed(vec![dm, (starred(2, 7), Some(20))]);
      assert_eq!(ids(&list, DirectSectionKind::Favorites), vec![2, 1]);
      assert!(ids(&list, DirectSectionKind::Conversations).is_empty());
    }

    #[test]
    fn a_channel_without_a_star_stays_in_its_space() {
      let mut plain = starred(2, 7);
      plain.is_favorite = false;
      assert!(listed(vec![(plain, Some(1))]).sections.is_empty());
    }

    #[test]
    fn handed_over_twice_it_is_one_row() {
      let list = listed(vec![(starred(2, 7), Some(1)), (starred(2, 7), Some(1))]);
      assert_eq!(ids(&list, DirectSectionKind::Favorites), vec![2]);
    }

    #[test]
    fn the_unread_count_is_the_channels_own_and_is_not_summed_twice() {
      let mut channel = starred(2, 7);
      channel.unread_count = 4;
      let list = listed(vec![(channel.clone(), None)]);
      let row = &list.sections[0].conversations[0];
      assert_eq!(row.unread_count, 4);
      // The space's rail counts it once; the Chats tab counts DMs only (bottom_nav), so the number
      // of the channel appears in the sum of both exactly once.
      let direct: Vec<Conversation> = list
        .sections
        .iter()
        .flat_map(|section| {
          section
            .conversations
            .iter()
            .filter(|c| c.kind.is_direct_like())
            .cloned()
        })
        .collect();
      assert_eq!(badge_count(&direct), 0);
      assert_eq!(RailBadge::of(&[channel]).count, 4);
    }

    #[test]
    fn the_chips_for_people_and_groups_leave_channels_out_but_unread_keeps_them() {
      let mut channel = starred(2, 7);
      channel.unread_count = 1;
      let entries = vec![(channel, None)];
      let with = |chip| build_direct(entries.clone(), DmSort::Recent, 0, "", chip);
      assert!(with(HomeFilter::People).sections.is_empty());
      assert!(with(HomeFilter::Groups).sections.is_empty());
      assert_eq!(
        ids(&with(HomeFilter::Unread), DirectSectionKind::Favorites),
        vec![2]
      );
    }

    #[test]
    fn the_text_filter_finds_a_starred_channel_by_name() {
      let mut channel = starred(2, 7);
      channel.name = "Launch".into();
      let list = build_direct(vec![(channel, None)], DmSort::Recent, 0, "laun", HomeFilter::All);
      assert_eq!(ids(&list, DirectSectionKind::Favorites), vec![2]);
    }
  }

  /// #1151: direct messages and the channels of all spaces in one list.
  mod one_list {
    use super::*;

    fn channel(id: u64, space: u64) -> Conversation {
      let mut channel = conversation(id, SidebarCategory::Channels, 0, false);
      channel.maybe_space = Some(id_from_raw::<SpaceId>(space));
      channel
    }

    fn options() -> ListOptions<'static> {
      ListOptions {
        sort: DmSort::Recent,
        show_unreads_group: false,
        maybe_held_open: None,
        filter: "",
        chip: HomeFilter::All,
        max_rows: 0,
        maybe_space_order: None,
      }
    }

    fn rows(list: &DirectList) -> Vec<(DirectSectionKind, u64)> {
      list
        .sections
        .iter()
        .flat_map(|section| {
          section
            .conversations
            .iter()
            .map(|conversation| (section.kind, crate::model::raw_id(conversation.id)))
        })
        .collect()
    }

    #[test]
    fn dms_and_channels_of_all_spaces_mix_by_recency() {
      let list = build_list(
        vec![
          direct(1, 0, Some(20)),
          (channel(2, 7), Some(30)),
          (channel(3, 8), Some(10)),
        ],
        &options(),
      );
      assert_eq!(
        rows(&list),
        [
          (DirectSectionKind::Everything, 2),
          (DirectSectionKind::Everything, 1),
          (DirectSectionKind::Everything, 3)
        ]
      );
    }

    #[test]
    fn notes_then_favorites_then_the_rest_then_deactivated() {
      let mut notes = direct(1, 0, None);
      notes.0.is_notes_to_self = true;
      let mut starred = channel(2, 7);
      starred.is_favorite = true;
      let mut gone = direct(3, 0, Some(99));
      gone.0.has_only_deactivated_others = true;
      let list = build_list(
        vec![gone, direct(4, 0, Some(50)), (starred, None), notes],
        &options(),
      );
      assert_eq!(
        rows(&list),
        [
          (DirectSectionKind::Notes, 1),
          (DirectSectionKind::Favorites, 2),
          (DirectSectionKind::Everything, 4),
          (DirectSectionKind::Deactivated, 3)
        ]
      );
    }

    #[test]
    fn unread_ones_go_apart_only_when_the_setting_asks() {
      let mut muted = channel(3, 7);
      muted.unread_count = 9;
      muted.is_muted = true;
      let entries = vec![direct(1, 2, Some(1)), (channel(2, 7), Some(2)), (muted, Some(3))];
      let plain = build_list(entries.clone(), &options());
      assert_eq!(plain.sections.len(), 1);
      let apart = ListOptions {
        show_unreads_group: true,
        ..options()
      };
      assert_eq!(
        rows(&build_list(entries, &apart)),
        [
          (DirectSectionKind::Unreads, 1),
          (DirectSectionKind::Everything, 3),
          (DirectSectionKind::Everything, 2)
        ],
        "a muted channel without a mention waits for nobody"
      );
    }

    #[test]
    fn the_conversation_opened_while_unread_stays_in_unreads() {
      let held = ListOptions {
        show_unreads_group: true,
        maybe_held_open: Some(id_from_raw(2)),
        ..options()
      };
      assert_eq!(
        rows(&build_list(vec![(channel(2, 7), None)], &held)),
        [(DirectSectionKind::Unreads, 2)]
      );
    }

    #[test]
    fn the_channels_chip_and_the_text_filter_narrow_it() {
      let mut launch = channel(2, 7);
      launch.name = "Launch".into();
      let entries = vec![direct(1, 0, None), (launch, None), (channel(3, 8), None)];
      let channels = ListOptions {
        chip: HomeFilter::Channels,
        ..options()
      };
      assert_eq!(
        build_list(entries.clone(), &channels).sections[0]
          .conversations
          .len(),
        2
      );
      let people = ListOptions {
        chip: HomeFilter::People,
        ..options()
      };
      assert_eq!(
        rows(&build_list(entries.clone(), &people)),
        [(DirectSectionKind::Everything, 1)]
      );
      let named = ListOptions {
        filter: " LAUN",
        ..options()
      };
      assert_eq!(
        rows(&build_list(entries, &named)),
        [(DirectSectionKind::Everything, 2)]
      );
    }

    #[test]
    fn a_conversation_handed_over_twice_is_one_row() {
      let list = build_list(vec![(channel(2, 7), None), (channel(2, 7), None)], &options());
      assert_eq!(rows(&list).len(), 1);
    }

    /// What gets rendered doesn't follow the size of the workspace: filtering doesn't lift the cut.
    #[test]
    fn a_big_workspace_hands_out_one_page_and_counts_the_rest() {
      let entries: Vec<_> = (0..5000)
        .map(|id| (channel(id, id % 40), Some(id as TimestampMs)))
        .collect();
      let paged = ListOptions {
        max_rows: 100,
        ..options()
      };
      let list = build_list(entries.clone(), &paged);
      assert_eq!(rows(&list).len(), 100);
      assert_eq!(list.hidden_count, 4900);
      assert_eq!(rows(&list)[0].1, 4999, "the newest come first");
      let filtered = ListOptions { filter: "c", ..paged };
      assert_eq!(rows(&build_list(entries.clone(), &filtered)).len(), 100);
      assert_eq!(build_list(entries, &options()).hidden_count, 0);
    }

    #[test]
    fn the_cut_takes_the_sections_in_order() {
      let mut starred = channel(1, 7);
      starred.is_favorite = true;
      let paged = ListOptions {
        max_rows: 1,
        ..options()
      };
      let list = build_list(vec![(channel(2, 7), Some(9)), (starred, Some(1))], &paged);
      assert_eq!(rows(&list), [(DirectSectionKind::Favorites, 1)]);
      assert_eq!(list.hidden_count, 1);
    }

    #[test]
    fn the_layout_stays_spaces_for_preferences_from_before_it() {
      let stored = crate::prefs::Preferences::from_json(r#"{"sidebar":{"sort":"default"}}"#);
      assert_eq!(stored.sidebar.layout, crate::prefs::SidebarLayout::Spaces);
    }

    /// #1345: the list keeps the channels of a space together.
    mod by_space {
      use super::*;

      fn space(raw: u64) -> SpaceId {
        id_from_raw(raw)
      }

      fn grouped(order: &[SpaceId]) -> ListOptions<'_> {
        ListOptions {
          maybe_space_order: Some(order),
          ..options()
        }
      }

      #[test]
      fn direct_messages_then_each_space_in_the_given_order_each_in_the_list_order() {
        let order = [space(8), space(7)];
        let list = build_list(
          vec![
            direct(1, 0, Some(20)),
            (channel(2, 7), Some(30)),
            (channel(3, 8), Some(10)),
            (channel(4, 7), Some(40)),
            direct(5, 0, Some(50)),
          ],
          &grouped(&order),
        );
        assert_eq!(
          rows(&list),
          [
            (DirectSectionKind::Conversations, 5),
            (DirectSectionKind::Conversations, 1),
            (DirectSectionKind::Space(space(8)), 3),
            (DirectSectionKind::Space(space(7)), 4),
            (DirectSectionKind::Space(space(7)), 2),
          ]
        );
      }

      #[test]
      fn a_space_without_channels_in_the_list_has_no_section() {
        let order = [space(7), space(8), space(9)];
        let mut starred = channel(3, 8);
        starred.is_favorite = true;
        let list = build_list(vec![(channel(2, 7), None), (starred, None)], &grouped(&order));
        let kinds: Vec<_> = list.sections.iter().map(|section| section.kind).collect();
        assert_eq!(
          kinds,
          [DirectSectionKind::Favorites, DirectSectionKind::Space(space(7))],
          "space 8's only channel is a favorite, space 9 has none"
        );
      }

      #[test]
      fn notes_unreads_and_favorites_stay_above_the_groups_and_deactivated_last() {
        let order = [space(7)];
        let mut notes = direct(1, 0, None);
        notes.0.is_notes_to_self = true;
        let mut unread = channel(2, 7);
        unread.unread_count = 1;
        let mut starred = channel(3, 7);
        starred.is_favorite = true;
        let mut gone = direct(4, 0, Some(99));
        gone.0.has_only_deactivated_others = true;
        let options = ListOptions {
          show_unreads_group: true,
          ..grouped(&order)
        };
        let list = build_list(
          vec![
            gone,
            (channel(5, 7), None),
            (starred, None),
            (unread, None),
            notes,
            direct(6, 0, None),
          ],
          &options,
        );
        assert_eq!(
          rows(&list),
          [
            (DirectSectionKind::Notes, 1),
            (DirectSectionKind::Unreads, 2),
            (DirectSectionKind::Favorites, 3),
            (DirectSectionKind::Conversations, 6),
            (DirectSectionKind::Space(space(7)), 5),
            (DirectSectionKind::Deactivated, 4),
          ]
        );
      }

      #[test]
      fn a_channel_of_a_space_not_in_the_order_goes_after_the_groups() {
        let order = [space(7), space(7)];
        let list = build_list(
          vec![(channel(2, 9), Some(5)), (channel(3, 7), None)],
          &grouped(&order),
        );
        assert_eq!(
          rows(&list),
          [
            (DirectSectionKind::Space(space(7)), 3),
            (DirectSectionKind::Everything, 2)
          ],
          "and a space named twice is one group"
        );
      }

      #[test]
      fn filters_keep_the_groups() {
        let order = [space(7), space(8)];
        let channels = ListOptions {
          chip: HomeFilter::Channels,
          ..grouped(&order)
        };
        let list = build_list(
          vec![direct(1, 0, None), (channel(2, 7), None), (channel(3, 8), None)],
          &channels,
        );
        assert_eq!(
          rows(&list),
          [
            (DirectSectionKind::Space(space(7)), 2),
            (DirectSectionKind::Space(space(8)), 3)
          ]
        );
      }

      /// Grouping doesn't lift the page: the groups are cut in order like any section.
      #[test]
      fn a_big_workspace_still_hands_out_one_page() {
        let order: Vec<SpaceId> = (0..40).map(space).collect();
        let entries: Vec<_> = (0..5000)
          .map(|id| (channel(id, id % 40), Some(id as TimestampMs)))
          .collect();
        let paged = ListOptions {
          max_rows: 100,
          ..grouped(&order)
        };
        let list = build_list(entries, &paged);
        assert_eq!(rows(&list).len(), 100);
        assert_eq!(list.hidden_count, 4900);
        assert!(
          rows(&list)
            .iter()
            .all(|(kind, _)| *kind == DirectSectionKind::Space(space(0))),
          "the first page is the first space's"
        );
      }

      #[test]
      fn the_grouping_is_mixed_for_preferences_from_before_it() {
        let stored = crate::prefs::Preferences::from_json(r#"{"sidebar":{"layout":"list"}}"#);
        assert_eq!(stored.sidebar.list_grouping, crate::prefs::ListGrouping::Mixed);
        for grouping in crate::prefs::ListGrouping::ALL {
          assert_eq!(
            crate::prefs::ListGrouping::from_slug(grouping.slug()),
            Some(grouping)
          );
        }
      }
    }
  }

  mod direct_page {
    use super::*;

    fn names(list: &DirectList) -> Vec<Vec<&str>> {
      list
        .sections
        .iter()
        .map(|section| section.conversations.iter().map(|c| c.name.as_str()).collect())
        .collect()
    }

    fn sample() -> Vec<(Conversation, Option<TimestampMs>)> {
      let mut amy = direct(1, 0, Some(10));
      amy.0.name = "Amy".into();
      let mut zed = direct(2, 0, Some(30));
      zed.0.name = "zed".into();
      let mut bob = direct(3, 0, None);
      bob.0.name = "bob".into();
      let mut group = direct(4, 0, Some(20));
      group.0.name = "Amy, bob".into();
      group.0.kind = ConversationKind::Group;
      let channel = (conversation(5, SidebarCategory::Channels, 0, false), Some(99));
      vec![amy, zed, bob, group, channel]
    }

    fn sample_with_old() -> Vec<(Conversation, Option<TimestampMs>)> {
      let mut old = direct(11, 0, Some(50));
      old.0.name = "Amy (old)".into();
      old.0.has_only_deactivated_others = true;
      let mut conversations = sample();
      conversations.push(old);
      conversations
    }

    #[test]
    fn notes_to_self_are_pinned_above_everything() {
      let mut notes = direct(9, 0, None);
      notes.0.name = "Zoe".into();
      notes.0.member_ids = vec![me()];
      notes.0.is_notes_to_self = true;
      let mut conversations = sample();
      conversations.push(notes);
      let list = build_direct(conversations, DmSort::Recent, 0, "", HomeFilter::All);
      assert_eq!(list.sections[0].kind, DirectSectionKind::Notes);
      assert_eq!(names(&list)[0], ["Zoe"]);
    }

    #[test]
    fn a_dm_left_with_only_the_user_is_not_a_second_notes() {
      // The other person was deactivated: the user is the DM's only member, yet it stays a DM.
      let mut notes = direct(9, 0, None);
      notes.0.name = "Zoe".into();
      notes.0.is_notes_to_self = true;
      let mut orphan = direct(10, 0, None);
      orphan.0.name = "Former colleague".into();
      orphan.0.member_ids = vec![me()];
      let mut conversations = sample();
      conversations.extend([notes, orphan]);
      let list = build_direct(conversations, DmSort::Recent, 0, "", HomeFilter::All);
      assert_eq!(names(&list)[0], ["Zoe"]);
      assert!(names(&list)[1].contains(&"Former colleague"));
    }

    #[test]
    fn a_deactivated_accounts_dm_moves_out_of_the_list_to_the_end() {
      // "Amy (old)" is Amy's deactivated account, favorite and newest; the new account is active.
      let mut old = direct(11, 0, Some(50));
      old.0.name = "Amy (old)".into();
      old.0.is_favorite = true;
      old.0.has_only_deactivated_others = true;
      let mut conversations = sample();
      conversations.push(old);
      let list = build_direct(conversations, DmSort::Recent, 1, "", HomeFilter::All);
      let last = list.sections.last().expect("sections");
      assert_eq!(last.kind, DirectSectionKind::Deactivated);
      assert_eq!(names(&list).last().expect("names"), &["Amy (old)"]);
      // Not among the favorites or the active conversations, and the limit doesn't hide it.
      assert!(list.sections[..list.sections.len() - 1].iter().all(|section| {
        section
          .conversations
          .iter()
          .all(|c| !c.has_only_deactivated_others)
      }));
      // Searching still finds it, so its history stays reachable.
      let found = build_direct(sample_with_old(), DmSort::Recent, 0, "old", HomeFilter::All);
      assert_eq!(names(&found), [vec!["Amy (old)"]]);
    }

    #[test]
    fn a_conversation_handed_over_twice_is_one_row() {
      let mut notes = direct(9, 0, None);
      notes.0.is_notes_to_self = true;
      let mut conversations = sample();
      conversations.extend([notes.clone(), notes]);
      let list = build_direct(conversations, DmSort::Recent, 0, "", HomeFilter::All);
      assert_eq!(list.sections[0].conversations.len(), 1);
      let rows: Vec<_> = list
        .sections
        .iter()
        .flat_map(|section| &section.conversations)
        .collect();
      let ids: HashSet<_> = rows.iter().map(|conversation| conversation.id).collect();
      assert_eq!(ids.len(), rows.len());
    }

    #[test]
    fn newest_first_without_channels() {
      let list = build_direct(sample(), DmSort::Recent, 0, "", HomeFilter::All);
      assert_eq!(names(&list), [vec!["zed", "Amy, bob", "Amy", "bob"]]);
      assert_eq!(list.hidden_count, 0);
    }

    #[test]
    fn alphabetical_with_favorites_first() {
      let mut conversations = sample();
      conversations[1].0.is_favorite = true;
      let list = build_direct(conversations, DmSort::Alphabetical, 0, "", HomeFilter::All);
      assert_eq!(names(&list), [vec!["zed"], vec!["Amy", "Amy, bob", "bob"]]);
      assert_eq!(list.sections[0].kind, DirectSectionKind::Favorites);
    }

    #[test]
    fn the_limit_keeps_unread_ones() {
      let mut conversations = sample();
      conversations[2].0.unread_count = 1;
      let list = build_direct(conversations, DmSort::Recent, 1, "", HomeFilter::All);
      assert_eq!(names(&list), [vec!["zed", "bob"]]);
      assert_eq!(list.hidden_count, 2);
    }

    #[test]
    fn chips_pick_unread_people_or_groups() {
      let mut conversations = sample();
      conversations[2].0.unread_count = 1;
      conversations[0].0.unread_count = 3;
      conversations[0].0.is_muted = true;
      let unread = build_direct(conversations.clone(), DmSort::Recent, 1, "", HomeFilter::Unread);
      assert_eq!(
        names(&unread),
        [vec!["bob"]],
        "muted without a mention isn't unread; no limit"
      );
      let people = build_direct(conversations.clone(), DmSort::Recent, 0, "", HomeFilter::People);
      assert_eq!(names(&people), [vec!["zed", "Amy", "bob"]]);
      let groups = build_direct(conversations, DmSort::Recent, 0, "", HomeFilter::Groups);
      assert_eq!(names(&groups), [vec!["Amy, bob"]]);
    }

    #[test]
    fn filtering_ignores_case_and_the_limit() {
      let list = build_direct(sample(), DmSort::Recent, 1, " AMY ", HomeFilter::All);
      assert_eq!(names(&list), [vec!["Amy, bob", "Amy"]]);
      assert_eq!(list.hidden_count, 0);
      assert!(
        build_direct(sample(), DmSort::Recent, 0, "nobody", HomeFilter::All)
          .sections
          .is_empty()
      );
    }
  }

  #[test]
  fn a_space_badge_sums_the_unread_messages_of_its_conversations() {
    let mut engineering = conversation(1, SidebarCategory::Channels, 12, false);
    engineering.mention_count = 2;
    let general = conversation(2, SidebarCategory::Channels, 3, false);
    let read = conversation(3, SidebarCategory::Channels, 0, false);
    assert_eq!(
      RailBadge::of(&[engineering, general, read]),
      RailBadge {
        count: 15,
        mention_count: 2
      }
    );
    assert_eq!(RailBadge::of(&[]), RailBadge::default());
  }

  #[test]
  fn a_space_badge_counts_only_the_mentions_of_muted_conversations() {
    let mut muted = conversation(1, SidebarCategory::Channels, 40, false);
    muted.is_muted = true;
    assert_eq!(RailBadge::of(std::slice::from_ref(&muted)), RailBadge::default());
    muted.mention_count = 1;
    let unread = conversation(2, SidebarCategory::Channels, 2, false);
    assert_eq!(
      RailBadge::of(&[muted, unread]),
      RailBadge {
        count: 3,
        mention_count: 1
      }
    );
  }

  #[test]
  fn a_mention_counts_although_the_unread_count_lags_behind() {
    let mut lagging = conversation(1, SidebarCategory::Channels, 0, false);
    lagging.mention_count = 2;
    assert_eq!(RailBadge::of(&[lagging]).count, 2);
  }

  #[test]
  fn a_space_badge_saturates_instead_of_overflowing() {
    let huge = conversation(1, SidebarCategory::Channels, u32::MAX, false);
    let more = conversation(2, SidebarCategory::Channels, 5, false);
    assert_eq!(RailBadge::of(&[huge, more]).count, u32::MAX);
  }

  #[test]
  fn the_grouped_layout_splits_channels_by_space() {
    let (a, b) = (id_from_raw::<SpaceId>(1), id_from_raw::<SpaceId>(2));
    let mut list = vec![
      conversation(1, SidebarCategory::Channels, 0, false),
      conversation(2, SidebarCategory::Channels, 4, false),
      conversation(3, SidebarCategory::Integrations, 0, false),
      direct(4, 0, None).0,
      conversation(5, SidebarCategory::Channels, 0, true),
    ];
    list[0].maybe_space = Some(a);
    list[1].maybe_space = Some(b);
    list[2].maybe_space = Some(a);
    list[4].maybe_space = Some(b);
    let spaces = [
      (a, "Project A".to_owned()),
      (b, "Project B".to_owned()),
      (id_from_raw(3), "Empty".to_owned()),
    ];
    let grouped = build_grouped(&spaces, &list, &SidebarOptions::plain(true));

    let top: Vec<SectionKind> = grouped.top.iter().map(|section| section.kind).collect();
    assert_eq!(top, [SectionKind::Unreads, SectionKind::Favorites]);
    let names: Vec<&str> = grouped.spaces.iter().map(|group| group.name.as_str()).collect();
    assert_eq!(
      names,
      ["Project A"],
      "Project B only has an unread and a favorite channel, both on top"
    );
    assert_eq!(grouped.spaces[0].conversations.len(), 2);
  }

  #[test]
  fn unreads_group_can_be_disabled() {
    let list = vec![conversation(1, SidebarCategory::Channels, 3, false)];
    assert_eq!(
      build_sections(&list, &SidebarOptions::plain(false))[0].kind,
      SectionKind::Channels
    );
  }

  /// The signed-in user of the tests.
  fn me() -> UserId {
    id_from_raw(7)
  }

  fn direct(id: u64, unread: u32, maybe_at: Option<TimestampMs>) -> (Conversation, Option<TimestampMs>) {
    let mut dm = conversation(id, SidebarCategory::DirectMessages, unread, false);
    dm.kind = ConversationKind::Direct;
    (dm, maybe_at)
  }

  fn conversation(id: u64, category: SidebarCategory, unread: u32, is_favorite: bool) -> Conversation {
    Conversation {
      maybe_draft_preview: None,
      id: id_from_raw::<ConversationId>(id),
      maybe_space: None,
      kind: ConversationKind::Public,
      category,
      name: format!("c{id}"),
      topic: String::new(),
      member_ids: Vec::new(),
      unread_count: unread,
      mention_count: 0,
      is_favorite,
      is_muted: false,
      can_post: true,
      bookmarks: Vec::new(),
      is_catching_up: false,
      is_notes_to_self: false,
      is_archived: false,
      is_imported: false,
      maybe_permissions: None,
      has_only_deactivated_others: false,
      maybe_image_version: None,
    }
  }
}
