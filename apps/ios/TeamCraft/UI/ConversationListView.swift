import SwiftUI

/// The chats: sidebar of conversations (split view on iPad, stack on iPhone), search, settings.
struct ConversationListView: View {
  @Environment(AppModel.self) private var model
  let account: Account

  @State private var conversations: [ConversationItem] = []
  @State private var selection: String?
  @State private var query = ""
  @State private var hits: [SearchHitItem] = []
  @State private var isShowingSettings = false
  @State private var isShowingMeetings = false
  @State private var isShowingFilters = false
  @State private var filters = SearchFilters()

  var body: some View {
    NavigationSplitView {
      sidebar
        .navigationTitle("chats_title")
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
      } else {
        Text("no_conversation_selected").foregroundStyle(Theme.textMuted)
      }
    }
    .searchable(text: $query, prompt: Text("search_hint"))
    .task(id: model.changeCount) { conversations = await model.conversations() }
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
    .sheet(isPresented: $isShowingSettings) { SettingsView(account: account) }
    .sheet(isPresented: $isShowingMeetings) { MeetingsView(account: account) }
    .sheet(isPresented: $isShowingFilters) { SearchFiltersView(filters: $filters) }
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
      Text("no_conversations").foregroundStyle(Theme.textMuted)
    } else {
      List(selection: $selection) {
        ForEach(conversationSections(conversations)) { section in
          Section(header: Text(title(of: section.title))) {
            ForEach(section.conversations, id: \.id) { conversation in
              ConversationRow(conversation: conversation).tag(conversation.id)
            }
          }
        }
      }
      .listStyle(.sidebar)
    }
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

  var body: some View {
    HStack(spacing: 10) {
      Image(systemName: icon).frame(width: 22).foregroundStyle(Theme.textMuted)
      Text(conversation.name)
        .fontWeight(conversation.unreadCount > 0 ? .semibold : .regular)
        .lineLimit(1)
      Spacer()
      if conversation.mentionCount > 0 {
        CountBadge(count: conversation.mentionCount, isMention: true)
      } else if conversation.unreadCount > 0 {
        CountBadge(count: conversation.unreadCount)
      }
    }
    .opacity(conversation.isMuted ? 0.6 : 1)
  }

  private var icon: String {
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
