//! Pure grouping of conversations into sidebar sections: the channels of a space, and the
//! Home (DMs and group DMs live outside spaces).

use teamcraft_client_api::activity::{ActivityItem, ActivityKind};

use crate::model::{Conversation, ConversationId, ConversationKind, SidebarCategory, SpaceId, TimestampMs};
use crate::prefs::{DmSort, SidebarSort};

/// Sidebar sections of a space in display order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SectionKind {
  Unreads,
  Favorites,
  Channels,
  Integrations,
}

impl SectionKind {
  pub const ALL: [SectionKind; 4] = [Self::Unreads, Self::Favorites, Self::Channels, Self::Integrations];

  pub fn from_slug(slug: &str) -> Option<Self> {
    Self::ALL.into_iter().find(|kind| kind.slug() == slug)
  }

  pub fn slug(self) -> &'static str {
    match self {
      Self::Unreads => "unreads",
      Self::Favorites => "favorites",
      Self::Channels => "channels",
      Self::Integrations => "integrations",
    }
  }

  /// i18n key of the section title.
  pub fn title_key(self) -> &'static str {
    match self {
      Self::Unreads => "sidebar-unreads",
      Self::Favorites => "sidebar-favorites",
      Self::Channels => "sidebar-channels",
      Self::Integrations => "sidebar-integrations",
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
}

impl SidebarOptions {
  pub fn plain(show_unreads_group: bool) -> Self {
    Self {
      show_unreads_group,
      sort: SidebarSort::Default,
    }
  }
}

/// Groups the channels of a space: unread ones (optional), favorites, then the remaining by
/// category. Every conversation appears exactly once; empty sections are omitted. Direct messages
/// have their own page ([`build_direct`]); one passed in here goes into no section.
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
    .filter(|conversation| !conversation.kind.is_direct_like())
  {
    let kind = if options.show_unreads_group && conversation.unread_count > 0 && !conversation.is_muted {
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
    if options.sort == SidebarSort::Alphabetical && section.kind != SectionKind::Unreads {
      section
        .conversations
        .sort_by_key(|conversation| conversation.name.to_lowercase());
    }
  }
  sections.retain(|section| !section.conversations.is_empty());
  sections
}

/// The sections of Home, in display order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DirectSectionKind {
  Favorites,
  Conversations,
}

impl DirectSectionKind {
  /// i18n key of the section title.
  pub fn title_key(self) -> &'static str {
    match self {
      Self::Favorites => "sidebar-favorites",
      Self::Conversations => "sidebar-direct-messages",
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
}

impl HomeFilter {
  /// The chips, in display order ("All" is no chip selected).
  pub const CHIPS: [HomeFilter; 3] = [Self::Unread, Self::People, Self::Groups];

  /// i18n key of the chip.
  pub fn label_key(self) -> &'static str {
    match self {
      Self::All => "home-filter-all",
      Self::Unread => "home-filter-unread",
      Self::People => "home-filter-people",
      Self::Groups => "home-filter-groups",
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

/// Lists DMs and group DMs (each with the time of its newest message): favorites, then the rest,
/// both in `sort` order. Only those `chip` lets through and whose names contain `filter` (any case)
/// are kept; while filtering the limit doesn't apply. `limit` 0 shows all; unread conversations stay
/// visible beyond it.
pub fn build_direct(
  conversations: Vec<(Conversation, Option<TimestampMs>)>,
  sort: DmSort,
  limit: usize,
  filter: &str,
  chip: HomeFilter,
) -> DirectList {
  let needle = filter.trim().to_lowercase();
  let mut list: Vec<(Conversation, Option<TimestampMs>)> = conversations
    .into_iter()
    .filter(|(conversation, _)| conversation.kind.is_direct_like() && chip.matches(conversation))
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
  let (favorites, rest): (Vec<Conversation>, Vec<Conversation>) = list
    .into_iter()
    .map(|(conversation, _)| conversation)
    .partition(|conversation| conversation.is_favorite);
  let mut hidden_count = 0;
  let is_filtering = !needle.is_empty() || chip != HomeFilter::All;
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
    (DirectSectionKind::Favorites, favorites),
    (DirectSectionKind::Conversations, rest),
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

/// What a space rail entry shows for its conversations: a mention count, or a dot for unread
/// messages in conversations that aren't muted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RailBadge {
  pub mention_count: u32,
  pub has_unread: bool,
}

impl RailBadge {
  pub fn of(conversations: &[Conversation]) -> Self {
    Self {
      mention_count: conversations
        .iter()
        .map(|conversation| conversation.mention_count)
        .sum(),
      has_unread: conversations
        .iter()
        .any(|conversation| conversation.unread_count > 0 && !conversation.is_muted),
    }
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
      SectionKind::Channels | SectionKind::Integrations => channels.extend(section.conversations),
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
        at: 0,
        excerpt: String::new(),
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

  #[test]
  fn each_channel_appears_once_and_direct_messages_not_at_all() {
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
        SectionKind::Integrations
      ]
    );
    assert_eq!(
      sections
        .iter()
        .map(|section| section.conversations.len())
        .sum::<usize>(),
      3
    );
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
    };
    let names: Vec<String> = build_sections(&list, &options)[0]
      .conversations
      .iter()
      .map(|c| c.name.clone())
      .collect();
    assert_eq!(names, ["Amy", "zed"]);
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
  fn rail_badges_skip_muted_unread_but_count_mentions() {
    let mut muted = conversation(1, SidebarCategory::Channels, 4, false);
    muted.is_muted = true;
    muted.mention_count = 1;
    assert_eq!(
      RailBadge::of(std::slice::from_ref(&muted)),
      RailBadge {
        mention_count: 1,
        has_unread: false
      }
    );
    let unread = conversation(2, SidebarCategory::Channels, 2, false);
    assert!(RailBadge::of(&[muted, unread]).has_unread);
    assert_eq!(RailBadge::of(&[]), RailBadge::default());
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

  fn direct(id: u64, unread: u32, maybe_at: Option<TimestampMs>) -> (Conversation, Option<TimestampMs>) {
    let mut dm = conversation(id, SidebarCategory::DirectMessages, unread, false);
    dm.kind = ConversationKind::Direct;
    (dm, maybe_at)
  }

  fn conversation(id: u64, category: SidebarCategory, unread: u32, is_favorite: bool) -> Conversation {
    Conversation {
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
      maybe_image_version: None,
    }
  }
}
