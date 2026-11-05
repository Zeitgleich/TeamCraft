import SwiftUI

/// The chats: sidebar of conversations (split view on iPad, stack on iPhone), search, settings.
struct ConversationListView: View {
  @Environment(AppModel.self) private var model
  let account: Account

  @State private var conversations: [ConversationItem] = []
  /// The local store was read once: until then an empty list is not "no chats yet" (no flash of
  /// the empty state at start; the store answers at once, offline too).
  @State private var hasLoaded = false
  /// Guests can't: they find archived channels in the list instead. Offered until the core says.
  @State private var canBrowseChannels = true
  @State private var previews: [String: ChatPreview] = [:]
  @State private var selection: String?
  @State private var isShowingDrafts = false
  @State private var query = ""
  @State private var hits: [SearchHitItem] = []
  @State private var isShowingSettings = false
  @State private var isShowingMeetings = false
  @State private var isShowingFilters = false
  @State private var filters = SearchFilters()
  @State private var maybeBrowseTab: BrowseView.Tab?
  /// The space whose people, calls, meetings, saved messages and files are open (#954).
  @State private var maybeSpaceViews: SpaceChoice?
  /// DMs live outside spaces (as on desktop), so they have their own tab.
  @State private var tab: ChatTab = .home
  @State private var homeChip: HomeFilter = .all
  /// Takes effect at once: the view follows the stored value (#1151).
  @AppStorage(ChatListLayout.key) private var layoutRaw = ChatListLayout.spaces.rawValue
  @State private var dmFilter = ""
  /// A mute from the long-press menu the server didn't take (#903).
  @State private var isMuteFailed = false
  /// An entry-menu action the core or server refused (#1105, #322).
  @State private var maybeActionFailure: AppFailure?
  @State private var suspectDeletions: UInt32 = 0
  /// What the core says about leaving, per channel row and space header: the rule is the core's.
  @State private var leaveChoices: [String: LeaveChoice] = [:]
  @State private var spaceMenus: [String: SpaceMenuItem] = [:]
  @State private var maybeLeaving: LeaveTarget?
  /// A first sync's progress, shown under the title.
  @State private var maybeFirstSync: FirstSyncItem?

  var body: some View {
    NavigationSplitView {
      sidebar
        .safeAreaInset(edge: .top, spacing: 0) {
          VStack(spacing: 0) {
            OfflineBanner()
            if let progress = maybeFirstSync { FirstSyncBar(progress: progress) }
            NotificationPromptCard()
            if !isSearching && !conversations.isEmpty && !isList { tabPicker }
          }
        }
        .navigationTitle("chats_title")
        .alert("dnd_save_failed", isPresented: $isMuteFailed) {}
        .alert("deletions_suspect \(suspectDeletions)", isPresented: isSuspectDeletionsShown) {}
        .task(id: shownSections.count) {
          let count = await model.takeSuspectDeletions()
          if count > 0 { suspectDeletions = count }
        }
        .alert("entry_action_failed", isPresented: isActionFailureShown) {
        } message: {
          if let failure = maybeActionFailure { Text(failure.message) }
        }
        .confirmationDialog(leaveTitle, isPresented: isLeavingShown, titleVisibility: .visible) {
          Button(
            LocalizedStringKey(maybeLeaving?.isDeletion == true ? "entry_delete_channel_confirm" : "entry_leave_confirm"),
            role: .destructive
          ) {
            confirmLeave()
          }
        } message: {
          Text(leaveMessage)
        }
        .toolbar {
          ToolbarItem(placement: .topBarLeading) {
            Button { isShowingFilters = true } label: {
              Image(
                systemName: filters.isEmpty
                  ? "line.3.horizontal.decrease.circle" : "line.3.horizontal.decrease.circle.fill")
            }
            .accessibilityLabel(Text("search_filters_title"))
          }
          ToolbarItem(placement: .topBarLeading) {
            Button { isShowingDrafts = true } label: { Image(systemName: "pencil.line") }
              .accessibilityLabel(Text("drafts_title"))
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
    // Once per account: a snapshot is too much to decode on every change for this.
    .task(id: account.userId) {
      canBrowseChannels = await model.browseSnapshot()?.canBrowseChannels ?? true
    }
    .task(id: model.changeCount) {
      conversations = await model.conversations()
      hasLoaded = true
      maybeFirstSync = await model.firstSync()
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
        // A starred channel opened from Home stays there, highlighted under Favorites (#1346).
        if tab == .home && selected.isListedInHome { return }
        tab = selected.isDirect ? .home : .spaces
      }
    }
    // Why an action on a request to join didn't go through, also for the open channel's banner;
    // the channel browser shows its own.
    .joinNoteAlert(model, isActive: maybeBrowseTab == nil)
    .sheet(isPresented: $isShowingSettings) { SettingsView(account: account) }
    .sheet(isPresented: $isShowingMeetings) { MeetingsView(account: account) }
    .sheet(isPresented: $isShowingDrafts) {
      DraftsAndSentView { place in
        model.openedPlace = place
        selection = place.conversationId
      }
    }
    .onChange(of: model.isShowingMeetingsFromLink) { _, isAsked in
      if isAsked {
        isShowingMeetings = true
        model.isShowingMeetingsFromLink = false
      }
    }
    .sheet(isPresented: $isShowingFilters) { SearchFiltersView(filters: $filters) }
    .sheet(item: $maybeSpaceViews) { choice in
      SpaceViewsView(
        account: account, spaces: spaceChoices(conversations), initialSpaceId: choice.id
      ) { conversationId in selection = conversationId }
    }
    .sheet(item: $maybeBrowseTab) { tab in
      BrowseView(tab: tab) { conversationId in selection = conversationId }
    }
  }

  /// The text and the filters in the core's query syntax.
  private var fullQuery: String { filters.query(for: query) }

  private var isSearching: Bool { !fullQuery.isEmpty }

  private var isList: Bool { layoutRaw == ChatListLayout.list.rawValue }

  /// Where the chips and the filter belong: Home, and always in the list layout.
  private var isHomeShown: Bool { isList || tab == .home }

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
        if isHomeShown {
          // Decided where the channels are (#799): in the list layout, here too.
          if isList { JoinRequestSection() }
          ScheduledAttentionSection()
          GuestNoticeSection()
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
          Section(
            header: sectionHeader(section)
              .contextMenu { spaceMenu(for: section) }
              .task(id: section.id) { await loadSpaceMenu(for: section) }
          ) {
            ForEach(section.conversations, id: \.id) { conversation in
              ConversationRow(
                conversation: conversation, maybePreview: previews[conversation.id], showsSpace: isHomeShown)
                .tag(conversation.id)
                .swipeActions(edge: .leading) { readButton(for: conversation) }
                .swipeActions(edge: .trailing) { favoriteButton(for: conversation) }
                .task(id: conversation.id) { await loadLeaveChoice(for: conversation) }
                .contextMenu {
                  readButton(for: conversation)
                  muteMenu(for: conversation)
                  levelMenu(for: conversation)
                  favoriteButton(for: conversation)
                  copyLinkButton(for: conversation)
                  leaveButton(for: conversation)
                  deleteButton(for: conversation)
                }
            }
          }
        }
        if isList && dmFilter.isEmpty && homeChip == .all { listSpacesSection }
      }
      .listStyle(.sidebar)
    }
  }

  /// The spaces under the list layout's list: a space's menu and browsing for more are here, since
  /// no header of a space is left to hold them (#1151).
  private var listSpacesSection: some View {
    Section("chats_tab_spaces") {
      ForEach(listedSpaces(conversations)) { section in
        Menu {
          spaceMenu(for: section)
        } label: {
          HStack {
            if let spaceId = section.conversations.compactMap(\.maybeSpaceId).first {
              SpacePictureView(
                spaceId: spaceId, maybeVersion: section.conversations.compactMap(\.maybeSpaceImageVersion).first)
            }
            Text(title(of: section.title))
          }
        }
        .task(id: section.id) { await loadSpaceMenu(for: section) }
      }
      Button("browse_channels", systemImage: "plus") { maybeBrowseTab = .channels }
    }
  }

  private var shownSections: [ConversationSection] {
    if isList {
      return listSections(
        conversations, lastActivity: previews.mapValues(\.sentDate), filter: dmFilter, chip: homeChip)
    }
    switch tab {
    case .spaces: return channelSections(conversations, listsArchived: !canBrowseChannels)
    case .home:
      return directSections(
        conversations, lastActivity: previews.mapValues(\.sentDate), filter: dmFilter, chip: homeChip)
    }
  }

  /// Unread / People / Groups; tapping the selected chip again shows everything.
  private var homeChips: some View {
    HStack {
      ForEach(isList ? HomeFilter.listChips : HomeFilter.chips, id: \.self) { chip in
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
    case .channels: return "channels"
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

  /// All messages, mentions only or nothing, besides the mute time (#1105).
  private func levelMenu(for conversation: ConversationItem) -> some View {
    Menu {
      ForEach(NotifyLevel.allCases) { level in
        Button(LocalizedStringKey(level.titleKey)) {
          Task {
            if !(await model.setConversationLevel(conversation.id, level: level.wire)) { isMuteFailed = true }
          }
        }
      }
    } label: {
      Label("entry_notifications", systemImage: "bell.badge")
    }
  }

  @ViewBuilder
  private func copyLinkButton(for conversation: ConversationItem) -> some View {
    if let link = model.shareLink(forConversation: conversation.id) {
      Button("copy_link", systemImage: "link") { UIPasteboard.general.string = link }
    }
  }

  /// Only where the core allows it; the last admin sees why it is off, before the tap.
  @ViewBuilder
  private func leaveButton(for conversation: ConversationItem) -> some View {
    switch leaveChoices[conversation.id] ?? .hidden {
    case .hidden:
      EmptyView()
    case .allowed:
      Button("entry_leave_channel", systemImage: "rectangle.portrait.and.arrow.right", role: .destructive) {
        maybeLeaving = .channel(id: conversation.id, name: conversation.name)
      }
    case .lastMember:
      Button("entry_leave_channel", systemImage: "rectangle.portrait.and.arrow.right", role: .destructive) {
        maybeLeaving = .lastMember(id: conversation.id, name: conversation.name)
      }
    case .lastAdmin:
      Button(
        LocalizedStringKey(
          conversation.canDeleteChannel ? "entry_leave_last_channel_admin_or_delete" : "entry_leave_last_channel_admin"
        ),
        systemImage: "rectangle.portrait.and.arrow.right"
      ) {}
        .disabled(true)
    }
  }

  /// Only for those who may; the confirmation says what happens to whom.
  @ViewBuilder
  private func deleteButton(for conversation: ConversationItem) -> some View {
    if conversation.canDeleteChannel {
      Button("entry_delete_channel", systemImage: "trash", role: .destructive) {
        maybeLeaving = .deletion(id: conversation.id, name: conversation.name)
      }
    }
  }

  private func loadLeaveChoice(for conversation: ConversationItem) async {
    guard !conversation.isDirect else { return }
    leaveChoices[conversation.id] = await model.conversationLeaveChoice(conversation.id)
  }

  /// A section's title; a space's opens the views of that space.
  @ViewBuilder
  private func sectionHeader(_ section: ConversationSection) -> some View {
    if case .space(let name) = section.title, let spaceId = section.conversations.compactMap(\.maybeSpaceId).first {
      Button {
        maybeSpaceViews = SpaceChoice(
          id: spaceId, name: name, maybeImageVersion: section.conversations.compactMap(\.maybeSpaceImageVersion).first)
      } label: {
        HStack {
          SpacePictureView(
            spaceId: spaceId, maybeVersion: section.conversations.compactMap(\.maybeSpaceImageVersion).first)
          Text(title(of: section.title))
          Spacer()
          Image(systemName: "chevron.right")
        }
      }
      .accessibilityHint(Text("space_views_open"))
    } else {
      Text(title(of: section.title))
    }
  }

  /// Mute (its channels follow), mark all read and leave: the menu of a space's header.
  @ViewBuilder
  private func spaceMenu(for section: ConversationSection) -> some View {
    if case .space(let name) = section.title, let spaceId = section.conversations.compactMap(\.maybeSpaceId).first {
      let menu = spaceMenus[spaceId]
      Menu {
        Button("mute_end", systemImage: "bell") { setSpaceMuted(spaceId, until: nil) }
        ForEach(MutePreset.allCases) { preset in
          Button(preset.title) { setSpaceMuted(spaceId, until: preset.mutedUntil(now: Date(), calendar: .current)) }
        }
      } label: {
        Label("entry_mute_space", systemImage: "bell.slash")
      }
      if menu?.hasUnread ?? section.conversations.contains(where: { $0.unreadCount > 0 || $0.mentionCount > 0 }) {
        Button("entry_mark_all_read", systemImage: "envelope.open") {
          Task { maybeActionFailure = await model.markSpaceRead(spaceId) }
        }
      }
      switch menu?.leave ?? .hidden {
      case .hidden:
        EmptyView()
      // `space_leave` doesn't answer `.lastMember`; the confirmation covers it either way.
      case .allowed, .lastMember:
        Button("entry_leave_space", systemImage: "rectangle.portrait.and.arrow.right", role: .destructive) {
          maybeLeaving = .space(id: spaceId, name: name)
        }
      case .lastAdmin:
        Button("entry_leave_last_space_admin", systemImage: "rectangle.portrait.and.arrow.right") {}
          .disabled(true)
      }
      if menu?.canDelete == true {
        Button("entry_delete_space", systemImage: "trash", role: .destructive) {
          maybeLeaving = .spaceDeletion(id: spaceId, name: name, channels: menu?.channelCount ?? 0)
        }
      }
    }
  }

  private func loadSpaceMenu(for section: ConversationSection) async {
    guard case .space = section.title, let spaceId = section.conversations.compactMap(\.maybeSpaceId).first else {
      return
    }
    spaceMenus[spaceId] = await model.spaceMenu(spaceId)
  }

  private func setSpaceMuted(_ spaceId: String, until maybeDate: Date?) {
    Task {
      if !(await model.setSpaceMuted(spaceId, until: maybeDate)) { isMuteFailed = true }
    }
  }

  private var isActionFailureShown: Binding<Bool> {
    Binding(get: { maybeActionFailure != nil }, set: { _ in maybeActionFailure = nil })
  }

  private var isLeavingShown: Binding<Bool> {
    Binding(get: { maybeLeaving != nil }, set: { if !$0 { maybeLeaving = nil } })
  }

  private var isSuspectDeletionsShown: Binding<Bool> {
    Binding(get: { suspectDeletions > 0 }, set: { if !$0 { suspectDeletions = 0 } })
  }

  private var leaveTitle: LocalizedStringKey {
    switch maybeLeaving {
    case .channel(_, let name), .lastMember(_, let name): return "entry_leave_channel_title \(name)"
    case .deletion(_, let name), .spaceDeletion(_, let name, _): return "entry_delete_channel_title \(name)"
    case .space(_, let name): return "entry_leave_space_title \(name)"
    case nil: return ""
    }
  }

  private var leaveMessage: LocalizedStringKey {
    switch maybeLeaving {
    case .space: return "entry_leave_space_text"
    case .lastMember: return "entry_leave_last_member_text"
    case .deletion: return "entry_delete_channel_text"
    case .spaceDeletion(_, _, let channels): return "entry_delete_space_text \(channels)"
    case .channel, nil: return "entry_leave_channel_text"
    }
  }

  private func confirmLeave() {
    guard let target = maybeLeaving else { return }
    maybeLeaving = nil
    Task {
      switch target {
      case .channel(let id, _): maybeActionFailure = await model.leaveChannel(id)
      case .lastMember(let id, _): maybeActionFailure = await model.leaveChannel(id, deletesIfLast: true)
      case .deletion(let id, _): maybeActionFailure = await model.deleteChannel(id)
      case .spaceDeletion(let id, _, _): maybeActionFailure = await model.deleteSpace(id)
      case .space(let id, _): maybeActionFailure = await model.leaveSpace(id)
      }
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
    case .archived: return "section_archived"
    case .space(let name): return LocalizedStringKey(name)
    case .channels: return "channels"
    case .directMessages: return "direct_messages"
    case .everything: return "list_conversations"
    }
  }
}

struct ConversationRow: View {
  let conversation: ConversationItem
  var maybePreview: ChatPreview?
  /// Names the space after a channel's name, where the list is not that space's own (#1152).
  var showsSpace = false

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
          if showsSpace, let space = conversation.maybeSpaceName {
            // After the name on the same line, so every row is as high as the others; it shortens
            // first (#1346).
            Text(verbatim: space)
              .font(.caption)
              .foregroundStyle(Theme.textMuted)
              .lineLimit(1)
              .truncationMode(.tail)
              .layoutPriority(-1)
          }
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
    // A draft stands in for the newest message: it's what the user owes this conversation (#1220).
    if let draft = conversation.maybeDraftPreview {
      Label {
        Text("draft_preview \(draft)")
      } icon: {
        Image(systemName: "pencil.line")
      }
      .labelStyle(.titleAndIcon)
    } else if let preview = maybePreview {
      switch (preview.sender, preview.content) {
      case (_, .noText): Text("chat_preview_event")
      case (.none, .placeholder(let placeholder)): Text(PlaceholderLine.text(placeholder))
      case (.me, .placeholder(let placeholder)):
        Text("chat_preview_sender \(String(localized: "you")) \(PlaceholderLine.text(placeholder))")
      case (.other(let name), .placeholder(let placeholder)):
        Text("chat_preview_sender \(name) \(PlaceholderLine.text(placeholder))")
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
            Text(verbatim: hit.message.shownAuthorName).font(.subheadline.weight(.semibold))
            Text(hit.message.body).lineLimit(3)
          }
        }
        .buttonStyle(.plain)
      }
    }
  }
}

/// What a leave confirmation is about.
private enum LeaveTarget {
  case channel(id: String, name: String)
  /// Nobody else is in the channel: leaving deletes it.
  case lastMember(id: String, name: String)
  case deletion(id: String, name: String)
  case spaceDeletion(id: String, name: String, channels: UInt32)
  case space(id: String, name: String)

  var isDeletion: Bool {
    switch self {
    case .deletion, .spaceDeletion: return true
    case .channel, .lastMember, .space: return false
    }
  }
}

/// A conversation's notification level, as the server names it.
private enum NotifyLevel: String, CaseIterable, Identifiable {
  case all, mentions, none

  var id: String { rawValue }
  var wire: String { rawValue }

  var titleKey: String {
    switch self {
    case .all: return "notify_all"
    case .mentions: return "notify_mentions"
    case .none: return "notify_none"
    }
  }
}

/// A first sync's progress under the title: what is loading, how far in conversations, and the bar.
/// Numbers from the core; a spinner until it has one. Gone while the connection is down, where the
/// banner above says why nothing moves.
struct FirstSyncBar: View {
  let progress: FirstSyncItem
  @Environment(AppModel.self) private var model
  private let network = NetworkMonitor.shared

  var body: some View {
    if !ConnectionNotice.isDown(isOnline: network.isOnline, connection: model.connection) {
      VStack(spacing: 4) {
        HStack {
          Text(progress.stage == .newest ? "first_sync_newest" : "first_sync_older")
          Spacer()
          if progress.total > 0 {
            Text("first_sync_count \(Int64(progress.done)) \(Int64(progress.total))")
              .monospacedDigit()
          } else {
            ProgressView().controlSize(.mini)
          }
        }
        .font(.footnote)
        .foregroundStyle(.secondary)
        if let fraction = progress.maybeFraction {
          ProgressView(value: Double(fraction))
            .progressViewStyle(.linear)
            .animation(.smooth, value: fraction)
        }
      }
      .padding(.horizontal)
      .padding(.vertical, 6)
      .accessibilityElement(children: .combine)
    }
  }
}
