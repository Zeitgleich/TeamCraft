import SwiftUI

/// The chats: sidebar of conversations (split view on iPad, stack on iPhone), search, settings.
struct ConversationListView: View {
  @Environment(AppModel.self) private var model
  let account: Account

  @State private var conversations: [ConversationItem] = []
  /// The local store was read once: until then an empty list is not "no chats yet" (no flash of
  /// the empty state at start; the store answers at once, offline too).
  @State private var hasLoaded = false
  @State private var previews: [String: ChatPreview] = [:]
  @State private var selection: String?
  @State private var query = ""
  @State private var hits: [SearchHitItem] = []
  @State private var isShowingSettings = false
  @State private var isShowingMeetings = false
  @State private var isShowingFilters = false
  @State private var filters = SearchFilters()
  @State private var maybeBrowseTab: BrowseView.Tab?
  /// DMs live outside spaces (as on desktop), so they have their own tab.
  @State private var tab: ChatTab = .home
  @State private var homeChip: HomeFilter = .all
  @State private var dmFilter = ""
  /// A mute from the long-press menu the server didn't take (#903).
  @State private var isMuteFailed = false

  var body: some View {
    NavigationSplitView {
      sidebar
        .safeAreaInset(edge: .top, spacing: 0) {
          VStack(spacing: 0) {
            OfflineBanner()
            NotificationPromptCard()
            if !isSearching && !conversations.isEmpty { tabPicker }
          }
        }
        .navigationTitle("chats_title")
        .alert("dnd_save_failed", isPresented: $isMuteFailed) {}
        .toolbar {
          ToolbarItem(placement: .topBarLeading) {
            Button { isShowingFilters = true } label: {
              Image(
                systemName: filters.isEmpty
                  ? "line.3.horizontal.decrease.circle" : "line.3.horizontal.decrease.circle.fill")
            }
            .accessibilityLabel(Text("search_filters_title"))
          }
          ToolbarItemGroup(placement: .topBarTrailing) {
            Button { maybeBrowseTab = .people } label: { Image(systemName: "square.and.pencil") }
              .accessibilityLabel(Text("new_chat"))
            Button { isShowingMeetings = true } label: { Image(systemName: "calendar") }
              .accessibilityLabel(Text("meetings_title"))
            Button { isShowingSettings = true } label: { Image(systemName: "gearshape") }
              .accessibilityLabel(Text("settings_title"))
          }
        }
    } detail: {
      if let conversation = conversations.first(where: { $0.id == selection }) {
        ChatPane(conversation: conversation, threadId: nil, account: account)
          .id(conversation.id)
      } else if let selection {
        // Opened from a notification before the sync brought it (issue #1065).
        PendingConversationView(conversationId: selection)
          .id(selection)
      } else {
        Text("no_conversation_selected").foregroundStyle(Theme.textMuted)
      }
    }
    .searchable(text: $query, prompt: Text("search_hint"))
    .task(id: model.changeCount) {
      conversations = await model.conversations()
      hasLoaded = true
      previews = await model.chatPreviews(conversations, myId: account.userId)
    }
    .task(id: fullQuery) {
      // Debounced: a new keystroke cancels this task before the search runs.
      try? await Task.sleep(for: .milliseconds(250))
      guard !Task.isCancelled else { return }
      hits = await model.search(fullQuery)
    }
    .onChange(of: model.openConversationId) { _, id in
      if let id {
        selection = id
        model.openConversationId = nil
      }
    }
    // A conversation opened from elsewhere (notification, link, new message) shows in its own tab.
    .onChange(of: selection) { _, id in
      if let selected = conversations.first(where: { $0.id == id }) {
        tab = selected.isDirect ? .home : .spaces
      }
    }
    // Why an action on a request to join didn't go through, also for the open channel's banner;
    // the channel browser shows its own.
    .joinNoteAlert(model, isActive: maybeBrowseTab == nil)
    .sheet(isPresented: $isShowingSettings) { SettingsView(account: account) }
    .sheet(isPresented: $isShowingMeetings) { MeetingsView(account: account) }
    .sheet(isPresented: $isShowingFilters) { SearchFiltersView(filters: $filters) }
    .sheet(item: $maybeBrowseTab) { tab in
      BrowseView(tab: tab) { conversationId in selection = conversationId }
    }
  }

  /// The text and the filters in the core's query syntax.
  private var fullQuery: String { filters.query(for: query) }

  private var isSearching: Bool { !fullQuery.isEmpty }

  @ViewBuilder
  private var sidebar: some View {
    if isSearching {
      SearchResultsView(hits: hits, query: fullQuery) { hit in
        query = ""
        filters = SearchFilters()
        selection = hit.message.conversationId
      }
    } else if conversations.isEmpty {
      if hasLoaded {
        EmptyChatsView { tab in maybeBrowseTab = tab }
      } else {
        Color.clear
      }
    } else {
      List(selection: $selection) {
        if tab == .home {
          TextField("dm_filter", text: $dmFilter)
            .textFieldStyle(.roundedBorder)
            .autocorrectionDisabled()
          homeChips
          if shownSections.isEmpty {
            let isUnfiltered = dmFilter.isEmpty && homeChip == .all
            Text(LocalizedStringKey(isUnfiltered ? "dm_empty" : "dm_filter_empty")).foregroundStyle(Theme.textMuted)
            if isUnfiltered {
              Button("dm_new", systemImage: "square.and.pencil") { maybeBrowseTab = .people }
            }
          }
        } else {
          // Admins asking to join a channel the user owns: decided where the channels are (#799).
          JoinRequestSection()
          if shownSections.isEmpty {
            Text("chats_no_channels").foregroundStyle(Theme.textMuted)
          }
        }
        ForEach(shownSections) { section in
          Section(header: Text(title(of: section.title))) {
            ForEach(section.conversations, id: \.id) { conversation in
              ConversationRow(conversation: conversation, maybePreview: previews[conversation.id])
                .tag(conversation.id)
                .swipeActions(edge: .leading) { readButton(for: conversation) }
                .swipeActions(edge: .trailing) { favoriteButton(for: conversation) }
                .contextMenu {
                  readButton(for: conversation)
                  muteMenu(for: conversation)
                  favoriteButton(for: conversation)
                }
            }
          }
        }
      }
      .listStyle(.sidebar)
    }
  }

  private var shownSections: [ConversationSection] {
    switch tab {
    case .spaces: return channelSections(conversations)
    case .home:
      return directSections(
        conversations, lastActivity: previews.mapValues(\.sentDate), filter: dmFilter, chip: homeChip)
    }
  }

  /// Unread / People / Groups; tapping the selected chip again shows everything.
  private var homeChips: some View {
    HStack {
      ForEach(HomeFilter.chips, id: \.self) { chip in
        Button(LocalizedStringKey(chipKey(chip))) { homeChip = homeChip == chip ? .all : chip }
          .buttonStyle(.bordered)
          .tint(homeChip == chip ? Theme.accent : .secondary)
          .accessibilityAddTraits(homeChip == chip ? .isSelected : [])
      }
    }
  }

  private func chipKey(_ chip: HomeFilter) -> String {
    switch chip {
    case .all: return "home_filter_all"
    case .unread: return "home_filter_unread"
    case .people: return "home_filter_people"
    case .groups: return "home_filter_groups"
    }
  }

  /// Home and spaces, each with what is new in it.
  private var tabPicker: some View {
    Picker("chats_title", selection: $tab) {
      ForEach(ChatTab.allCases, id: \.self) { tab in
        Text(tabLabel(tab)).tag(tab)
      }
    }
    .pickerStyle(.segmented)
    .padding(.horizontal)
    .padding(.vertical, 6)
  }

  /// "Home (3)": channels count only mentions (and the requests to join waiting for a decision),
  /// DMs every unread message.
  private func tabLabel(_ tab: ChatTab) -> String {
    let isDirect = tab == .home
    let name = isDirect ? String(localized: "home_title") : String(localized: "chats_tab_spaces")
    let badge = TabBadge(conversations.filter { $0.isDirect == isDirect })
    let count = isDirect ? max(badge.mentions, badge.unread) : badge.mentions + model.joinRequests.count
    if count > 0 { return "\(name) (\(count))" }
    return badge.unread > 0 ? "\(name) •" : name
  }

  /// Read when unread; otherwise unread again from the newest message.
  @ViewBuilder
  private func readButton(for conversation: ConversationItem) -> some View {
    if conversation.unreadCount > 0 {
      Button("chat_mark_read", systemImage: "envelope.open") {
        Task { await model.markRead(conversationId: conversation.id) }
      }
      .tint(Theme.accent)
    } else if let preview = previews[conversation.id] {
      Button("chat_mark_unread", systemImage: "envelope.badge") {
        Task {
          await model.perform(.markUnread(conversationId: conversation.id, fromMessageId: preview.messageId))
        }
      }
      .tint(Theme.accent)
    }
  }

  /// Mute for a while, or unmute: the same presets as the conversation's bell (#903).
  private func muteMenu(for conversation: ConversationItem) -> some View {
    Menu {
      if conversation.isMuted {
        Button("mute_end", systemImage: "bell") { setMuted(conversation, until: nil) }
      }
      ForEach(MutePreset.allCases) { preset in
        Button(preset.title) { setMuted(conversation, until: preset.mutedUntil(now: Date(), calendar: .current)) }
      }
    } label: {
      Label("mute_title", systemImage: conversation.isMuted ? "bell.slash" : "bell")
    }
  }

  private func setMuted(_ conversation: ConversationItem, until maybeDate: Date?) {
    Task {
      if !(await model.setConversationMuted(conversation.id, until: maybeDate)) { isMuteFailed = true }
    }
  }

  private func favoriteButton(for conversation: ConversationItem) -> some View {
    Button(
      LocalizedStringKey(conversation.isFavorite ? "favorite_remove" : "favorite_add"),
      systemImage: conversation.isFavorite ? "star.slash" : "star"
    ) {
      Task { await model.setFavorite(conversationId: conversation.id, isFavorite: !conversation.isFavorite) }
    }
    .tint(.orange)
  }

  private func title(of title: SectionTitle) -> LocalizedStringKey {
    switch title {
    case .favorites: return "favorites"
    case .space(let name): return LocalizedStringKey(name)
    case .channels: return "channels"
    case .directMessages: return "direct_messages"
    }
  }
}

struct ConversationRow: View {
  let conversation: ConversationItem
  var maybePreview: ChatPreview?

  private var isUnread: Bool { conversation.unreadCount > 0 }

  var body: some View {
    HStack(spacing: 10) {
      Image(systemName: icon).frame(width: 22).foregroundStyle(Theme.textMuted)
        .overlay(alignment: .bottomTrailing) {
          if let presence = conversation.maybePeerPresence {
            Circle()
              .fill(presence.color)
              .frame(width: 8, height: 8)
              .accessibilityLabel(Text(presence.label))
          }
        }
      VStack(alignment: .leading, spacing: 2) {
        HStack {
          Text(conversation.listName)
            .fontWeight(isUnread ? .semibold : .regular)
            .lineLimit(1)
          if conversation.isMuted {
            Image(systemName: "bell.slash").font(.caption).foregroundStyle(Theme.textMuted)
              .accessibilityLabel(Text("chat_muted"))
          }
          Spacer()
          if let badge = rowBadge(
            unreadCount: conversation.unreadCount, mentionCount: conversation.mentionCount, isMuted: conversation.isMuted)
          {
            RowCountBadge(badge: badge, isMuted: conversation.isMuted)
          }
          if let preview = maybePreview {
            Text(listTime(preview.sentDate))
              .font(.caption)
              .foregroundStyle(isUnread ? Theme.accent : Theme.textMuted)
          }
        }
        HStack {
          previewText
            .font(.subheadline)
            .foregroundStyle(isUnread ? Theme.text : Theme.textMuted)
            .lineLimit(1)
          Spacer()
        }
      }
    }
    .opacity(conversation.isMuted ? 0.6 : 1)
  }

  @ViewBuilder
  private var previewText: some View {
    if let preview = maybePreview {
      switch (preview.sender, preview.content) {
      case (_, .noText): Text("chat_preview_event")
      case (.none, .text(let text)): Text(text)
      case (.me, .text(let text)): Text("chat_preview_sender \(String(localized: "you")) \(text)")
      case (.other(let name), .text(let text)): Text("chat_preview_sender \(name) \(text)")
      }
    } else {
      Text(conversation.topic)
    }
  }

  /// Today: the time; this week: the weekday; older: the date.
  private func listTime(_ date: Date) -> String {
    if Calendar.current.isDateInToday(date) { return date.formatted(date: .omitted, time: .shortened) }
    if let days = Calendar.current.dateComponents([.day], from: date, to: .now).day, days < 7 {
      return date.formatted(.dateTime.weekday(.abbreviated))
    }
    return date.formatted(date: .numeric, time: .omitted)
  }

  private var icon: String {
    if conversation.isNotesToSelf { return "bookmark" }
    switch conversation.kind {
    case .direct: return "person"
    case .group: return "person.2"
    case .integration: return "bolt"
    case .private: return "lock"
    default: return "number"
    }
  }
}

struct SearchResultsView: View {
  let hits: [SearchHitItem]
  let query: String
  let onSelect: (SearchHitItem) -> Void

  var body: some View {
    if hits.isEmpty {
      Text("search_empty").foregroundStyle(Theme.textMuted)
    } else {
      List(hits, id: \.message.id) { hit in
        Button { onSelect(hit) } label: {
          VStack(alignment: .leading, spacing: 2) {
            Text(hit.conversationName).font(.caption).foregroundStyle(Theme.textMuted)
            Text(hit.message.authorName).font(.subheadline.weight(.semibold))
            Text(hit.message.body).lineLimit(3)
          }
        }
        .buttonStyle(.plain)
      }
    }
  }
}
