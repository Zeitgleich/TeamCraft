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
  @State private var runningCalls: [String: Int] = [:]
  @State private var selection: String?
  @State private var isShowingDrafts = false
  @State private var query = ""
  @State private var hits: [SearchHitItem] = []
  @State private var isShowingSettings = false
  @State private var isShowingMeetings = false
  @State private var isShowingFilters = false
  @State private var filters = SearchFilters()
  @State private var maybeBrowseTab: BrowseView.Tab?
  /// The space whose channels "Browse channels" shows first (#1526).
  @State private var maybeBrowseSpaceId: String?
  /// The space whose people, calls, meetings, saved messages and files are open (#954).
  @State private var maybeSpaceViews: SpaceChoice?
  /// The view the space's views open on: the space menu names each (#1526).
  @State private var spaceViewTab = SpaceViewTab.people
  /// The spaces the user belongs to (#1526), also those without a joined channel, from the same
  /// snapshot read as `canBrowseChannels`: again when the list's spaces change or the switcher
  /// opens, never per change (#1466).
  @State private var snapshotSpaces: [HomeSpace] = []
  @State private var isSwitchingSpace = false
  /// Spaces where the user may create channels, and whether new spaces (#1534): asked when the
  /// "New" menu or the switcher opens, so a role changed meanwhile counts.
  @State private var channelSpaceIds: Set<String> = []
  @State private var canCreateSpaces = false
  @State private var maybeNewChannelSpace: SpaceChoice?
  @State private var isCreatingSpace = false
  /// "New space" was picked in the switcher: its sheet comes once the switcher is gone.
  @State private var createsSpaceNext = false
  /// Bumped after a space was made here: it has no channel yet, so the list wouldn't read it.
  @State private var createdSpaces = 0
  /// The space Home of the spaces layout shows, remembered on this device as on Android.
  @AppStorage("home_selected_space") private var rememberedSpaceId = ""
  /// Home, DMs and Activity (#1488), as on Android.
  @State private var tab: ChatTab = .home
  /// Home's sections the user collapsed, on this device (keys: `ConversationSection.collapseKey`).
  @AppStorage("home_collapsed_sections") private var collapsedRaw = ""
  @State private var homeChip: HomeFilter = .all
  /// Takes effect at once: the view follows the stored value (#1151).
  @AppStorage(ChatListLayout.key) private var layoutRaw = ChatListLayout.spaces.rawValue
  @AppStorage(ChatListGrouping.key) private var groupingRaw = ChatListGrouping.mixed.rawValue
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
            // Any conversation with messages still to come (issue #1457).
            SyncingLine(isCatchingUp: conversations.contains(where: \.isCatchingUp))
            if let progress = maybeFirstSync { FirstSyncBar(progress: progress) }
            NotificationPromptCard()
          }
        }
        // The new-message button floats above the list, in the theme's color (#1488).
        .overlay(alignment: .bottomTrailing) {
          if !isSearching && tab != .activity { composeButton }
        }
        .safeAreaInset(edge: .bottom, spacing: 0) {
          if !isSearching && !conversations.isEmpty { bottomNavigation }
        }
        .navigationTitle(maybeHeaderSpace.map { Text($0.name) } ?? Text("chats_title"))
        .navigationBarTitleDisplayMode(maybeHeaderSpace == nil ? .automatic : .inline)
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
          // Drafts, scheduled messages and meetings are Home's quick-access cards; a new message is
          // the floating button (#1488).
          ToolbarItemGroup(placement: .topBarTrailing) {
            if let space = maybeHeaderSpace {
              // The selected space's views, browsing and actions, next to its name (#1526).
              Menu {
                spaceActions(spaceId: space.id, name: space.name, hasUnread: space.mentionCount > 0 || space.hasUnread)
              } label: {
                Image(systemName: "ellipsis.circle")
              }
              .accessibilityLabel(Text("space_options \(space.name)"))
              .task(id: space.id) { spaceMenus[space.id] = await model.spaceMenu(space.id) }
            }
            Button { isShowingSettings = true } label: { Image(systemName: "gearshape") }
              .accessibilityLabel(Text("settings_title"))
          }
          if let space = maybeHeaderSpace {
            ToolbarItem(placement: .principal) {
              SpaceSwitcherTitle(
                space: space, hasNewElsewhere: hasNewElsewhere(spaces, selectedId: space.id)
              ) { isSwitchingSpace = true }
            }
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
    // A snapshot is too much to decode on every change for this: once per account, and again when
    // the list's spaces change or the switcher opens (#1526).
    .task(
      id: SpacesRead(
        userId: account.userId, count: conversations.count, spaceIds: listedSpaceIds, isSwitching: isSwitchingSpace,
        created: createdSpaces)
    ) {
      guard let snapshot = await model.browseSnapshot() else { return }
      canBrowseChannels = snapshot.canBrowseChannels
      snapshotSpaces = (snapshot.spaces ?? []).map { HomeSpace(id: $0.id, name: $0.name, maybeImageVersion: $0.maybeImageVersion) }
      channelSpaceIds = Set((snapshot.spaces ?? []).filter(\.offersChannelCreation).map(\.id))
    }
    .task(id: isSwitchingSpace) {
      if canBrowseChannels { canCreateSpaces = await model.canCreateSpaces() }
    }
    .task(id: model.changeCount) {
      conversations = await model.conversations()
      // The chat list shows the core's rows: a signed-in start is done (#1521).
      Perf.chatListShown()
      runningCalls = await model.runningCalls()
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
    // A channel opened from elsewhere (notification, link) while the DMs show: Home has it. Home
    // lists DMs with something new too, so a DM opened from there stays highlighted there.
    .onChange(of: selection) { _, id in
      if let selected = conversations.first(where: { $0.id == id }), tab == .direct, isList || !selected.isDirect {
        tab = .home
      }
    }
    // Why an action on a request to join didn't go through, also for the open channel's banner;
    // the channel browser shows its own.
    .joinNoteAlert(model, isActive: maybeBrowseTab == nil)
    .sheet(isPresented: $isShowingSettings) { SettingsView(account: account) }
    .crashReportsNotice { isShowingSettings = true }
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
        account: account, spaces: spaces.map { SpaceChoice(id: $0.id, name: $0.name, maybeImageVersion: $0.maybeImageVersion) },
        initialSpaceId: choice.id, initialTab: spaceViewTab
      ) { conversationId in selection = conversationId }
    }
    .sheet(item: $maybeBrowseTab, onDismiss: { maybeBrowseSpaceId = nil }) { tab in
      BrowseView(tab: tab, maybeInitialSpaceId: maybeBrowseSpaceId) { conversationId in selection = conversationId }
    }
    .sheet(
      isPresented: $isSwitchingSpace,
      onDismiss: {
        if createsSpaceNext {
          createsSpaceNext = false
          isCreatingSpace = true
        }
      }
    ) {
      SpaceSwitcherView(
        spaces: spaces, selectedId: maybeSelectedSpace?.id, canBrowse: canBrowseChannels,
        onCreate: canCreateSpaces ? { createsSpaceNext = true } : nil
      ) { spaceId in
        rememberedSpaceId = spaceId
        tab = .home
      }
    }
    .sheet(item: $maybeNewChannelSpace) { space in
      NewChannelView(spaceId: space.id, spaceName: space.name) { conversationId in selection = conversationId }
    }
    .sheet(isPresented: $isCreatingSpace) {
      NewSpaceView { spaceId in
        createdSpaces += 1
        rememberedSpaceId = spaceId
        tab = .home
      }
    }
  }

  /// The text and the filters in the core's query syntax.
  private var fullQuery: String { filters.query(for: query) }

  private var isSearching: Bool { !fullQuery.isEmpty }

  private var isList: Bool { layoutRaw == ChatListLayout.list.rawValue }

  private var isBySpace: Bool { isList && groupingRaw == ChatListGrouping.bySpace.rawValue }

  /// The spaces the user belongs to, by name, with what is new in each (#1526).
  private var spaces: [HomeSpace] { homeSpaces(snapshotSpaces, conversations: conversations) }

  private var listedSpaceIds: Set<String> { Set(conversations.compactMap(\.maybeSpaceId)) }

  /// The space Home of the spaces layout shows; the list layout shows all of them.
  private var maybeSelectedSpace: HomeSpace? {
    isList ? nil : selectedSpace(spaces, remembered: rememberedSpaceId)
  }

  /// The space the header names: on Home of the spaces layout, not while searching.
  private var maybeHeaderSpace: HomeSpace? {
    shownTab == .home && !isSearching ? maybeSelectedSpace : nil
  }

  /// The tab on screen: the list layout has the DMs on Home and no tab of their own for them.
  private var shownTab: ChatTab { isList && tab == .direct ? .home : tab }

  /// Where the chips and the filter belong: the DMs, and Home in the list layout.
  private var isFilterShown: Bool { isList ? shownTab == .home : shownTab == .direct }

  private var collapsed: Set<String> { Set(collapsedRaw.split(separator: "\n").map(String.init)) }

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
        if shownTab == .home {
          quickAccessCards
          ScheduledAttentionSection()
          GuestNoticeSection()
        }
        if shownTab == .activity {
          // Admins asking to join a channel the user owns: they wait for a decision (#799).
          JoinRequestSection()
          if shownSections.isEmpty && model.joinRequests.isEmpty {
            ContentUnavailableView {
              Label("activity_empty_title", systemImage: "checkmark")
            } description: {
              Text("activity_empty")
            }
          }
        } else if isFilterShown {
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
        } else if shownSections.isEmpty {
          Text("spaces_empty").foregroundStyle(Theme.textMuted)
        }
        ForEach(shownSections) { section in
          let isFiltering = !dmFilter.isEmpty || homeChip != .all
          let isCollapsible = shownTab == .home && (!isList || (isBySpace && !isFiltering)) && section.collapseKey != nil
          let isCollapsed = isCollapsible && collapsed.contains(section.collapseKey ?? "")
          Section(
            header: sectionHeader(section, isCollapsible: isCollapsible, isCollapsed: isCollapsed)
              .contextMenu { spaceMenu(for: section) }
              .task(id: section.id) { await loadSpaceMenu(for: section) }
          ) {
            // A space without joined channels says so, and where to find some (#1526).
            if let spaceId = section.spaceId, section.conversations.isEmpty, !isCollapsed {
              Text("space_no_channels").foregroundStyle(Theme.textMuted)
              // Home of the spaces layout has the same button right below.
              if canBrowseChannels && section.isSpace {
                Button("browse_channels", systemImage: "plus") { browseChannels(in: spaceId) }
              }
            }
            ForEach(section.visibleRows(isCollapsed: isCollapsed, selectedId: selection), id: \.id) { conversation in
              ConversationRow(
                conversation: conversation, maybePreview: previews[conversation.id],
                // A space's section names it in its header, so its rows don't (#1345).
                showsSpace: section.title == .favorites || (isList && !section.isSpace),
                callCount: runningCalls[conversation.id] ?? 0,
                typingNames: typingNames(in: model.typingNow, conversation: conversation.id, thread: nil))
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
        if isList && !isBySpace && shownTab == .home && dmFilter.isEmpty && homeChip == .all { listSpacesSection }
        if (!isList || isBySpace) && shownTab == .home && canBrowseChannels {
          Button("browse_channels", systemImage: "plus") { browseChannels(in: maybeSelectedSpace?.id) }
        }
      }
      .listStyle(.sidebar)
      .animation(.snappy(duration: 0.25), value: collapsedRaw)
    }
  }

  /// The spaces under the list layout's list, also those without joined channels: a tap opens a
  /// space's views and menu (#1151, #1526).
  private var listSpacesSection: some View {
    Section("chats_tab_spaces") {
      ForEach(spaces) { space in
        Menu {
          spaceActions(spaceId: space.id, name: space.name, hasUnread: space.mentionCount > 0 || space.hasUnread)
        } label: {
          HStack(spacing: 12) {
            SpaceMark(spaceId: space.id, name: space.name, maybeImageVersion: space.maybeImageVersion)
            Text(space.name).lineLimit(1)
            Spacer(minLength: 8)
            SpaceBadgeMark(space: space)
            Image(systemName: "ellipsis").foregroundStyle(.secondary)
          }
          .contentShape(Rectangle())
        }
        .accessibilityHint(Text("space_options \(space.name)"))
        .task(id: space.id) { spaceMenus[space.id] = await model.spaceMenu(space.id) }
      }
      Button("browse_channels", systemImage: "plus") { browseChannels(in: nil) }
    }
  }

  private func browseChannels(in maybeSpaceId: String?) {
    maybeBrowseSpaceId = maybeSpaceId
    maybeBrowseTab = .channels
  }

  private func openSpaceView(_ spaceId: String, name: String, tab: SpaceViewTab) {
    spaceViewTab = tab
    maybeSpaceViews = SpaceChoice(
      id: spaceId, name: name, maybeImageVersion: spaces.first { $0.id == spaceId }?.maybeImageVersion)
  }

  private var shownSections: [ConversationSection] {
    if isList {
      let sections = listSections(
        conversations, lastActivity: previews.mapValues(\.sentDate), filter: dmFilter, chip: homeChip, bySpace: isBySpace)
      // While filtering, only matches count: no empty spaces.
      let isFiltering = !dmFilter.isEmpty || homeChip != .all
      return isBySpace && !isFiltering ? withEmptySpaces(sections, spaces: spaces) : sections
    }
    let lastActivity = previews.mapValues(\.sentDate)
    switch shownTab {
    case .home:
      // Without spaces (yet), Home lists what there is.
      if let space = maybeSelectedSpace {
        return spaceHomeSections(
          conversations, spaceId: space.id, lastActivity: lastActivity, listsArchived: !canBrowseChannels)
      }
      return homeSections(conversations, lastActivity: lastActivity, listsArchived: !canBrowseChannels)
    case .direct: return directSections(conversations, lastActivity: lastActivity, filter: dmFilter, chip: homeChip)
    case .activity:
      let waiting = activityConversations(conversations, lastActivity: lastActivity)
      return waiting.isEmpty ? [] : [ConversationSection(title: .activity, conversations: waiting)]
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

  /// Home, DMs and Activity at the bottom (#1488), each with what is new in it: a number for what
  /// is addressed to the user (mentions, DMs, requests to join), a dot for new channel messages.
  private var bottomNavigation: some View {
    HStack(spacing: 0) {
      ForEach(ChatTab.allCases.filter { !(isList && $0 == .direct) }, id: \.self) { item in
        let badge = tabBadge(item)
        Button {
          tab = item
        } label: {
          VStack(spacing: 2) {
            Image(systemName: tabSymbol(item))
              .symbolVariant(shownTab == item ? .fill : .none)
              .font(.system(size: 20))
              .frame(height: 26)
              .overlay(alignment: .topTrailing) { TabBadgeMark(count: badge.count, hasNew: badge.hasNew) }
            Text(tabLabel(item)).font(.caption2)
          }
          .frame(maxWidth: .infinity, minHeight: 49)
          .foregroundStyle(shownTab == item ? Theme.accent : Theme.textMuted)
          .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(Text(tabLabel(item)))
        .accessibilityValue(badge.count > 0 ? Text("chat_unread \(badge.count)") : Text(verbatim: ""))
        .accessibilityAddTraits(shownTab == item ? .isSelected : [])
      }
    }
    .background(.bar)
    .overlay(alignment: .top) { Divider() }
    .sensoryFeedback(.selection, trigger: tab)
  }

  private func tabLabel(_ tab: ChatTab) -> LocalizedStringKey {
    switch tab {
    case .home: return "home_title"
    case .direct: return "nav_direct"
    case .activity: return "nav_activity"
    }
  }

  private func tabSymbol(_ tab: ChatTab) -> String {
    switch tab {
    case .home: return "house"
    case .direct: return "bubble.left.and.bubble.right"
    case .activity: return "bell"
    }
  }

  /// Channels and Activity count only mentions; DMs (and the list layout's Home) every unread message.
  private func tabBadge(_ tab: ChatTab) -> (count: Int, hasNew: Bool) {
    let direct = TabBadge(conversations.filter(\.isDirect))
    let channels = TabBadge(conversations.filter { !$0.isDirect })
    switch tab {
    case .home where isList:
      return (max(direct.mentions + channels.mentions, direct.unread), channels.unread > 0)
    case .home: return (channels.mentions, channels.unread > 0)
    case .direct: return (max(direct.mentions, direct.unread), false)
    case .activity: return (direct.mentions + channels.mentions + model.joinRequests.count, false)
    }
  }

  /// Bottom right in the theme's color (#1488). On Home a menu of what can be started (#1534): a
  /// message, a channel in the space Home shows, a space, browsing; on the DMs a new message.
  @ViewBuilder
  private var composeButton: some View {
    if shownTab == .home {
      Menu {
        Button("dm_new", systemImage: "square.and.pencil") { maybeBrowseTab = .people }
        if let space = maybeChannelSpace {
          Button("create_channel_in_menu \(space.name)", systemImage: "number") {
            maybeNewChannelSpace = SpaceChoice(id: space.id, name: space.name)
          }
        }
        if canCreateSpaces {
          Button("create_space", systemImage: "square.grid.2x2") { isCreatingSpace = true }
        }
        if canBrowseChannels {
          Button("browse_channels", systemImage: "magnifyingglass") { browseChannels(in: maybeChannelSpace?.id) }
        }
      } label: {
        composeMark(symbol: "plus")
      }
      .task { if canBrowseChannels { canCreateSpaces = await model.canCreateSpaces() } }
      .padding(16)
      .accessibilityLabel(Text("create_new"))
    } else {
      Button { maybeBrowseTab = .people } label: { composeMark(symbol: "square.and.pencil") }
        .buttonStyle(.plain)
        .padding(16)
        .accessibilityLabel(Text("new_chat"))
    }
  }

  private func composeMark(symbol: String) -> some View {
    Image(systemName: symbol)
      .font(.title2.weight(.semibold))
      .foregroundStyle(.white)
      .frame(width: 56, height: 56)
      .background(Theme.accent, in: .rect(cornerRadius: 16))
      .shadow(color: .black.opacity(0.18), radius: 8, y: 4)
  }

  /// Where "New channel" of the menu creates one: the space Home shows, or in the list layout the
  /// only space where the user may; else each space's menu has it.
  private var maybeChannelSpace: HomeSpace? {
    if let selected = maybeSelectedSpace { return channelSpaceIds.contains(selected.id) ? selected : nil }
    let creatable = spaces.filter { channelSpaceIds.contains($0.id) }
    return isList && creatable.count == 1 ? creatable.first : nil
  }

  /// The quick-access cards on top of Home (#1488): drafts, scheduled messages and meetings, each
  /// with a one-line status from state the list has anyway (#1466); a card with something waiting
  /// is tinted.
  private var quickAccessCards: some View {
    let draftCount = conversations.filter { $0.maybeDraftPreview != nil }.count
    return ScrollView(.horizontal, showsIndicators: false) {
      HStack(spacing: 8) {
        QuickAccessCard(
          symbol: "pencil.line", title: "home_entry_drafts",
          status: draftCount > 0 ? "quick_drafts_count \(draftCount)" : "quick_drafts_none",
          isHighlighted: draftCount > 0
        ) { isShowingDrafts = true }
        if let scheduled = model.maybeScheduledCount {
          let needsAttention = model.scheduledAttentionCount > 0
          NavigationLink { ScheduledMessagesView() } label: {
            QuickAccessCardLabel(
              symbol: "clock", title: "home_entry_scheduled",
              status: needsAttention
                ? "quick_scheduled_attention" : scheduled > 0 ? "quick_scheduled_count \(scheduled)" : "quick_scheduled_none",
              isHighlighted: needsAttention || scheduled > 0)
          }
          .buttonStyle(.plain)
        }
        QuickAccessCard(
          symbol: "calendar", title: "home_entry_meetings", status: "quick_meetings_status", isHighlighted: false
        ) { isShowingMeetings = true }
      }
      .padding(.vertical, 4)
    }
    .listRowInsets(EdgeInsets(top: 4, leading: 16, bottom: 4, trailing: 16))
    .listRowBackground(Color.clear)
  }

  private func toggleCollapsed(_ key: String) {
    var keys = collapsed
    if keys.contains(key) { keys.remove(key) } else { keys.insert(key) }
    collapsedRaw = keys.sorted().joined(separator: "\n")
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

  /// A section's title; a space's opens the views of that space. On Home, a tap on the title
  /// collapses the section (#1488); a collapsed one keeps what is new.
  @ViewBuilder
  private func sectionHeader(_ section: ConversationSection, isCollapsible: Bool, isCollapsed: Bool) -> some View {
    if section.title == .activity {
      // Activity's one list needs no title.
      EmptyView()
    } else if isCollapsible, let key = section.collapseKey {
      HStack(spacing: 8) {
        Button { toggleCollapsed(key) } label: {
          HStack(spacing: 8) {
            Image(systemName: "chevron.down")
              .font(.footnote.weight(.semibold))
              .rotationEffect(.degrees(isCollapsed ? -90 : 0))
            if section.isSpace, let spaceId = section.spaceId {
              SpacePictureView(spaceId: spaceId, maybeVersion: section.spaceImageVersion)
            }
            Text(title(of: section.title))
            Spacer(minLength: 0)
          }
          .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(Text(title(of: section.title)))
        .accessibilityHint(
          isCollapsed
            ? Text("section_expand \(name(of: section.title))") : Text("section_collapse \(name(of: section.title))"))
        .accessibilityAddTraits(.isHeader)
        // The space's views and actions behind a visible button (#1526), not only a long-press.
        if case .space(let name) = section.title, let spaceId = section.spaceId {
          spaceOptionsButton(spaceId: spaceId, name: name, section: section)
        }
      }
    } else if case .space(let name) = section.title, let spaceId = section.spaceId {
      HStack {
        SpacePictureView(spaceId: spaceId, maybeVersion: section.spaceImageVersion)
        Text(title(of: section.title))
        Spacer()
        spaceOptionsButton(spaceId: spaceId, name: name, section: section)
      }
    } else {
      Text(title(of: section.title))
    }
  }

  private func spaceOptionsButton(spaceId: String, name: String, section: ConversationSection) -> some View {
    Menu {
      spaceMenu(for: section)
    } label: {
      Image(systemName: "ellipsis.circle").frame(minWidth: 44, minHeight: 32)
    }
    .buttonStyle(.plain)
    .accessibilityLabel(Text("space_options \(name)"))
  }

  /// The menu of a space's section header (and its long-press).
  @ViewBuilder
  private func spaceMenu(for section: ConversationSection) -> some View {
    if case .space(let name) = section.title, let spaceId = section.spaceId {
      spaceActions(
        spaceId: spaceId, name: name,
        hasUnread: section.conversations.contains(where: { $0.unreadCount > 0 || $0.mentionCount > 0 }))
    }
  }

  /// A space's views and browsing its channels (#1526), then mute (its channels follow), mark all
  /// read and leave. `hasUnread` stands in until the core's menu is read.
  @ViewBuilder
  private func spaceActions(spaceId: String, name: String, hasUnread: Bool) -> some View {
    let menu = spaceMenus[spaceId]
    Section {
      ForEach(SpaceViewTab.allCases) { view in
        Button { openSpaceView(spaceId, name: name, tab: view) } label: {
          Label(view.title, systemImage: view.symbol)
        }
      }
      if canBrowseChannels {
        Button("browse_channels", systemImage: "magnifyingglass") { browseChannels(in: spaceId) }
      }
      if channelSpaceIds.contains(spaceId) {
        Button("channel_create", systemImage: "number") {
          maybeNewChannelSpace = SpaceChoice(id: spaceId, name: name)
        }
      }
    }
    Menu {
      Button("mute_end", systemImage: "bell") { setSpaceMuted(spaceId, until: nil) }
      ForEach(MutePreset.allCases) { preset in
        Button(preset.title) { setSpaceMuted(spaceId, until: preset.mutedUntil(now: Date(), calendar: .current)) }
      }
    } label: {
      Label("entry_mute_space", systemImage: "bell.slash")
    }
    if menu?.hasUnread ?? hasUnread {
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

  private func loadSpaceMenu(for section: ConversationSection) async {
    guard case .space = section.title, let spaceId = section.spaceId else { return }
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

  /// The section's title as plain text, for what VoiceOver says about it.
  private func name(of title: SectionTitle) -> String {
    switch title {
    case .space(let name): return name
    case .favorites: return String(localized: "favorites")
    case .archived: return String(localized: "section_archived")
    case .channels, .spaceChannels: return String(localized: "channels")
    case .directMessages: return String(localized: "direct_messages")
    case .everything: return String(localized: "list_conversations")
    case .unreadDirect: return String(localized: "section_unread_direct")
    case .activity: return String(localized: "nav_activity")
    }
  }

  private func title(of title: SectionTitle) -> LocalizedStringKey {
    switch title {
    case .favorites: return "favorites"
    case .archived: return "section_archived"
    case .space(let name): return LocalizedStringKey(name)
    case .channels, .spaceChannels: return "channels"
    case .directMessages: return "direct_messages"
    case .everything: return "list_conversations"
    case .unreadDirect: return "section_unread_direct"
    case .activity: return "nav_activity"
    }
  }
}

/// When the chat list reads the snapshot for its spaces (#1526): per account, and when the list's
/// spaces or its size change or the switcher opens; never on every change (#1466).
private struct SpacesRead: Hashable {
  let userId: String
  let count: Int
  let spaceIds: Set<String>
  let isSwitching: Bool
  let created: Int
}

struct ConversationRow: View {
  let conversation: ConversationItem
  var maybePreview: ChatPreview?
  /// Names the space after a channel's name, where the list is not that space's own (#1152).
  var showsSpace = false
  /// People in the call running here; 0 without one (issue #1352).
  var callCount = 0
  /// Who types here right now (#882): the row says so instead of the preview.
  var typingNames: [String] = []

  private var isUnread: Bool { conversation.unreadCount > 0 }

  var body: some View {
    HStack(spacing: 10) {
      leading
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
          if conversation.isWithBot { BotBadge() }
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
          if callCount > 0 {
            Label("\(callCount)", systemImage: "phone.fill")
              .font(.caption2.weight(.semibold))
              .foregroundStyle(.white)
              .padding(.horizontal, 6)
              .padding(.vertical, 1)
              .background(Color.green, in: Capsule())
              .accessibilityLabel(Text("call_participants \(callCount)"))
          }
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
    if let line = typingRowText(typingNames, isDirect: conversation.kind == .direct) {
      // Someone types: that stands in for the preview and the draft, until they stop (#882).
      HStack(spacing: 6) {
        TypingDots()
        Text(line).italic()
      }
      .foregroundStyle(Theme.accent)
    } else if let draft = conversation.maybeDraftPreview {
      // A draft stands in for the newest message: it's what the user owes this conversation (#1220).
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

  /// A person's DM shows their colour and initials (the hue desktop shows); the rest a symbol.
  @ViewBuilder
  private var leading: some View {
    if conversation.kind == .direct, !conversation.isNotesToSelf {
      Text(AvatarView.initials(of: conversation.name))
        .font(.system(size: 11, weight: .semibold))
        .foregroundStyle(.white)
        .frame(width: 26, height: 26)
        .background(conversation.avatarColor, in: Circle())
        .accessibilityHidden(true)
    } else {
      Image(systemName: icon).frame(width: 22).foregroundStyle(Theme.textMuted)
    }
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

extension ConversationItem {
  /// A one-to-one DM with a bot or integration account.
  var isWithBot: Bool { kind == .direct && maybePeer?.isBot == true }

  /// The avatar colour of the other person: the core's hue, else derived from the ID or the name.
  var avatarColor: Color {
    if let peer = maybePeer { return PersonColor.avatar(hue: peer.avatarHue) }
    return PersonColor.avatar(maybePeerId ?? id)
  }
}

/// The BOT marker after the name of a DM with a bot (as on web).
struct BotBadge: View {
  var body: some View {
    Text("badge_bot")
      .font(.caption2.weight(.semibold))
      .foregroundStyle(Theme.textMuted)
      .padding(.horizontal, 4)
      .padding(.vertical, 1)
      .background(Color(.tertiarySystemFill), in: RoundedRectangle(cornerRadius: 4))
      .lineLimit(1)
      .fixedSize()
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
            Text(hit.listName).font(.caption).foregroundStyle(Theme.textMuted)
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

/// A quick-access card on top of Home (#1488): icon, title and a one-line status.
private struct QuickAccessCard: View {
  let symbol: String
  let title: LocalizedStringKey
  let status: LocalizedStringKey
  let isHighlighted: Bool
  let action: () -> Void

  var body: some View {
    Button(action: action) {
      QuickAccessCardLabel(symbol: symbol, title: title, status: status, isHighlighted: isHighlighted)
    }
    .buttonStyle(.plain)
  }
}

private struct QuickAccessCardLabel: View {
  let symbol: String
  let title: LocalizedStringKey
  let status: LocalizedStringKey
  let isHighlighted: Bool

  var body: some View {
    VStack(alignment: .leading, spacing: 4) {
      Image(systemName: symbol).font(.body.weight(.semibold))
      Text(title).font(.subheadline.weight(.semibold)).lineLimit(1)
      Text(status)
        .font(.caption)
        .fontWeight(isHighlighted ? .semibold : .regular)
        .foregroundStyle(isHighlighted ? Theme.accent : Theme.textMuted)
        .lineLimit(1)
    }
    .frame(width: 132, alignment: .leading)
    .padding(12)
    .background(
      isHighlighted ? Theme.accentSoft : Color(.secondarySystemGroupedBackground),
      in: .rect(cornerRadius: 16))
    .contentShape(.rect(cornerRadius: 16))
    .accessibilityElement(children: .combine)
  }
}

/// A tab's mark: the number of what is addressed to the user, or a dot for something new.
private struct TabBadgeMark: View {
  let count: Int
  let hasNew: Bool

  var body: some View {
    if count > 0 {
      Text(count > 99 ? "99+" : "\(count)")
        .font(.caption2.weight(.bold))
        .foregroundStyle(.white)
        .padding(.horizontal, 5)
        .frame(minWidth: 18, minHeight: 18)
        .background(.red, in: .capsule)
        .offset(x: 12, y: -6)
        .accessibilityHidden(true)
    } else if hasNew {
      Circle().fill(Theme.textMuted).frame(width: 8, height: 8).offset(x: 6, y: -2).accessibilityHidden(true)
    }
  }
}
