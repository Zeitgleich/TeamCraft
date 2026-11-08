import SwiftUI

/// The conversation a filters sheet is open for (a list's `sheet(item:)` needs an `Identifiable`).
struct ConversationFiltersTarget: Identifiable {
  let id: String
}

/// One conversation's notification filters (notifications.md, "Per-conversation filters"): whether
/// `@channel` counts as a mention, and quiet hours that mute the conversation at set times. The
/// server enforces both for pushes, so every device follows.
struct ConversationFiltersView: View {
  @Environment(AppModel.self) private var model
  @Environment(\.dismiss) private var dismiss
  @Environment(\.accessibilityReduceMotion) private var reduceMotion
  let conversationId: String

  @State private var filters = ConversationFilters.initial
  @State private var isLoaded = false
  @State private var isLoadFailed = false
  @State private var isSaving = false
  @State private var isSaveFailed = false
  /// Errors show once a save was refused, then follow the input (forms.md).
  @State private var wasRefused = false
  @AccessibilityFocusState private var focusedRange: UUID?

  private var problems: [UUID: QuietRangeProblem] {
    wasRefused ? QuietRangeProblem.problems(in: filters.quietWindows) : [:]
  }

  var body: some View {
    NavigationStack {
      ScrollViewReader { proxy in
        Form {
          if isLoadFailed {
            Section { Text("dnd_load_failed").foregroundStyle(Theme.danger) }
          }
          channelSection
          quietSection(proxy: proxy)
        }
        .disabled(!isLoaded)
        .navigationTitle("notify_filters_title")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
          ToolbarItem(placement: .cancellationAction) { Button("cancel") { dismiss() } }
          ToolbarItem(placement: .confirmationAction) {
            Button("dnd_save") { save(proxy: proxy) }.disabled(!isLoaded || isSaving)
          }
        }
      }
    }
    .task { await load() }
    .alert("dnd_save_failed", isPresented: $isSaveFailed) {}
  }

  // MARK: @channel

  private var channelSection: some View {
    Section {
      Picker("notify_channel_mentions", selection: channelChoice) {
        ForEach(ChannelMentionsChoice.allCases) { choice in
          Text(choice.title).tag(Optional(choice))
        }
      }
      .pickerStyle(.inline)
      .labelsHidden()
    } header: {
      Text("notify_channel_mentions")
    } footer: {
      VStack(alignment: .leading, spacing: 4) {
        Text("notify_channel_mentions_hint")
        if filters.channelMentions.maybeChoice == nil { Text("notify_channel_mentions_unknown_hint") }
      }
    }
  }

  /// `nil` while the server has a value from a newer version, until one is picked.
  private var channelChoice: Binding<ChannelMentionsChoice?> {
    Binding(
      get: { filters.channelMentions.maybeChoice },
      set: { if let choice = $0 { filters.channelMentions = .known(choice) } })
  }

  // MARK: Quiet hours

  private func quietSection(proxy: ScrollViewProxy) -> some View {
    Section {
      ForEach($filters.quietWindows) { $window in
        VStack(alignment: .leading, spacing: 4) {
          NavigationLink {
            DndWindowEditor(window: $window)
          } label: {
            DndWindowSummary(window: window)
          }
          .accessibilityFocused($focusedRange, equals: window.id)
          .fieldError(problems[window.id]?.message)
          FieldMessage(maybeError: problems[window.id]?.message)
        }
        .id(window.id)
      }
      .onDelete { offsets in filters.quietWindows.remove(atOffsets: offsets) }
      Button("dnd_add_range") { filters.quietWindows.append(.weeknights) }
        .disabled(!filters.canAddWindow)
    } header: {
      Text("notify_quiet_hours")
    } footer: {
      Text("notify_quiet_hours_hint \(TimeZone.current.identifier) \(DndSchedule.maxWindows)")
    }
  }

  // MARK: Actions

  private func load() async {
    guard let json = await model.conversationNotificationSettingsJSON(conversationId),
      let loaded = ConversationFilters.read(in: json)
    else {
      isLoadFailed = true
      return
    }
    filters = loaded
    isLoaded = true
    isLoadFailed = false
  }

  private func save(proxy: ScrollViewProxy) {
    // Every range is checked at once; the first invalid one takes the focus.
    if let first = QuietRangeProblem.firstInvalid(in: filters.quietWindows) {
      wasRefused = true
      scrollToMiddle(first, in: proxy, reduceMotion: reduceMotion)
      focusedRange = first
      return
    }
    isSaving = true
    Task {
      defer { isSaving = false }
      if await model.saveConversationFilters(conversationId, filters: filters) {
        dismiss()
      } else {
        isSaveFailed = true
      }
    }
  }
}

extension ChannelMentionsChoice {
  var title: LocalizedStringKey {
    switch self {
    case .followGeneral: return "notify_channel_mentions_default"
    case .notify: return "notify_channel_mentions_notify"
    case .ignore: return "notify_channel_mentions_ignore"
    }
  }
}

extension QuietRangeProblem {
  var message: LocalizedStringKey {
    switch self {
    case .noDays: return "notify_quiet_no_days"
    case .sameTime: return "notify_quiet_same_time"
    }
  }
}
