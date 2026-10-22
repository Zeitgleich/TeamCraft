//! Pure grouping of conversations into sidebar sections.

use crate::model::{Conversation, SidebarCategory, SpaceId};
use crate::prefs::SidebarSort;

/// Sidebar sections in display order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SectionKind {
  Unreads,
  Favorites,
  Channels,
  Integrations,
  DirectMessages,
}

impl SectionKind {
  pub fn slug(self) -> &'static str {
    match self {
      Self::Unreads => "unreads",
      Self::Favorites => "favorites",
      Self::Channels => "channels",
      Self::Integrations => "integrations",
      Self::DirectMessages => "direct-messages",
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
    }
  }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Section {
  pub kind: SectionKind,
  pub conversations: Vec<Conversation>,
  /// Direct messages cut off by the display limit.
  pub hidden_count: usize,
}

/// How the sidebar groups and orders conversations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SidebarOptions {
  pub show_unreads_group: bool,
  pub sort: SidebarSort,
  /// Direct messages to show; 0 shows all.
  pub dm_limit: usize,
}

impl SidebarOptions {
  pub fn plain(show_unreads_group: bool) -> Self {
    Self {
      show_unreads_group,
      sort: SidebarSort::Default,
      dm_limit: 0,
    }
  }
}

/// Groups conversations: unread ones (optional), favorites, then the remaining by category. Every
/// conversation appears exactly once; empty sections are omitted except the channel list itself.
pub fn build_sections(conversations: &[Conversation], options: &SidebarOptions) -> Vec<Section> {
  let mut sections: Vec<Section> = [
    SectionKind::Unreads,
    SectionKind::Favorites,
    SectionKind::Channels,
    SectionKind::Integrations,
    SectionKind::DirectMessages,
  ]
  .into_iter()
  .map(|kind| Section {
    kind,
    conversations: Vec::new(),
    hidden_count: 0,
  })
  .collect();

  for conversation in conversations {
    let kind = if options.show_unreads_group && conversation.unread_count > 0 && !conversation.is_muted {
      SectionKind::Unreads
    } else if conversation.is_favorite {
      SectionKind::Favorites
    } else {
      match conversation.category {
        SidebarCategory::Favorites => SectionKind::Favorites,
        SidebarCategory::Channels => SectionKind::Channels,
        SidebarCategory::Integrations => SectionKind::Integrations,
        SidebarCategory::DirectMessages => SectionKind::DirectMessages,
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
    if section.kind == SectionKind::DirectMessages
      && options.dm_limit > 0
      && section.conversations.len() > options.dm_limit
    {
      // Unread conversations stay visible even beyond the limit.
      let (keep, rest): (Vec<_>, Vec<_>) = section
        .conversations
        .drain(..)
        .enumerate()
        .partition(|(index, conv)| *index < options.dm_limit || conv.unread_count > 0);
      section.hidden_count = rest.len();
      section.conversations = keep.into_iter().map(|(_, conv)| conv).collect();
    }
  }
  sections.retain(|section| !section.conversations.is_empty());
  sections
}

/// The channels of one space in the grouped sidebar.
#[derive(Clone, Debug, PartialEq)]
pub struct SpaceGroup {
  pub space: SpaceId,
  pub name: String,
  /// Channels first, then integrations; unread and favorite ones are in [`GroupedSidebar::top`].
  pub conversations: Vec<Conversation>,
}

/// The "all spaces in one sidebar" layout: unread and favorite conversations of every space on top,
/// then one group per space, then the direct messages.
#[derive(Clone, Debug, PartialEq)]
pub struct GroupedSidebar {
  pub top: Vec<Section>,
  pub spaces: Vec<SpaceGroup>,
  pub direct: Vec<Section>,
}

/// Splits [`build_sections`] by space. `spaces` gives the order and names of the groups; a channel
/// of a space not listed there goes into no group. Spaces without channels are omitted.
pub fn build_grouped(
  spaces: &[(SpaceId, String)],
  conversations: &[Conversation],
  options: &SidebarOptions,
) -> GroupedSidebar {
  let mut top = Vec::new();
  let mut direct = Vec::new();
  let mut channels: Vec<Conversation> = Vec::new();
  for section in build_sections(conversations, options) {
    match section.kind {
      SectionKind::Unreads | SectionKind::Favorites => top.push(section),
      SectionKind::DirectMessages => direct.push(section),
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
  GroupedSidebar { top, spaces, direct }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::{ConversationId, ConversationKind, id_from_raw};

  #[test]
  fn each_conversation_appears_once() {
    let list = vec![
      conversation(1, SidebarCategory::Channels, 3, false),
      conversation(2, SidebarCategory::Channels, 0, true),
      conversation(3, SidebarCategory::DirectMessages, 0, false),
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

  #[test]
  fn sorting_and_dm_limit_apply() {
    let mut list = vec![
      conversation(1, SidebarCategory::DirectMessages, 0, false),
      conversation(2, SidebarCategory::DirectMessages, 0, false),
      conversation(3, SidebarCategory::DirectMessages, 0, false),
    ];
    list[0].name = "zed".into();
    list[1].name = "Amy".into();
    list[2].name = "bob".into();
    let options = SidebarOptions {
      show_unreads_group: true,
      sort: SidebarSort::Alphabetical,
      dm_limit: 2,
    };
    let sections = build_sections(&list, &options);
    let names: Vec<&str> = sections[0]
      .conversations
      .iter()
      .map(|c| c.name.as_str())
      .collect();
    assert_eq!(names, ["Amy", "bob"]);
    assert_eq!(sections[0].hidden_count, 1);
  }

  #[test]
  fn the_grouped_layout_splits_channels_by_space() {
    let (a, b) = (id_from_raw::<SpaceId>(1), id_from_raw::<SpaceId>(2));
    let mut list = vec![
      conversation(1, SidebarCategory::Channels, 0, false),
      conversation(2, SidebarCategory::Channels, 4, false),
      conversation(3, SidebarCategory::Integrations, 0, false),
      conversation(4, SidebarCategory::DirectMessages, 0, false),
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
    assert_eq!(grouped.direct[0].conversations.len(), 1);
  }

  #[test]
  fn unreads_group_can_be_disabled() {
    let list = vec![conversation(1, SidebarCategory::Channels, 3, false)];
    assert_eq!(
      build_sections(&list, &SidebarOptions::plain(false))[0].kind,
      SectionKind::Channels
    );
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
    }
  }
}
