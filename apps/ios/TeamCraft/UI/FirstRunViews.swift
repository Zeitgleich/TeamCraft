import SwiftUI

/// Asks for notifications once the account is set up, with a line on why, instead of the bare
/// system prompt right after sign-in. The system prompt only opens from the button; "Not now" is
/// remembered, the switch stays in Settings → Notifications.
struct NotificationPromptCard: View {
  static let answeredKey = "notificationPromptAnswered"

  @Environment(AppModel.self) private var model
  @AppStorage(Self.answeredKey) private var hasAnswered = false

  var body: some View {
    if model.pushStatus == .unknown && !hasAnswered {
      VStack(alignment: .leading, spacing: 8) {
        Label("notification_prompt_title", systemImage: "bell.fill")
          .font(.headline)
          .foregroundStyle(Theme.text)
        Text("notification_prompt_text").font(.subheadline).foregroundStyle(Theme.textMuted)
        HStack {
          Spacer()
          Button("not_now") { hasAnswered = true }
          Button("notification_prompt_allow") {
            hasAnswered = true
            Task { await model.enablePush() }
          }
          .buttonStyle(.borderedProminent)
          .tint(Theme.accent)
        }
      }
      .padding()
      .background(Theme.accentSoft)
      .clipShape(RoundedRectangle(cornerRadius: Theme.radius))
      .padding(.horizontal)
      .padding(.vertical, 4)
    }
  }
}

/// No conversations yet: what to do first.
struct EmptyChatsView: View {
  let onBrowse: (BrowseView.Tab) -> Void

  var body: some View {
    ContentUnavailableView {
      Label("no_conversations", systemImage: "bubble.left.and.bubble.right")
    } description: {
      Text("chats_empty_hint")
    } actions: {
      Button("browse_channels") { onBrowse(.channels) }
        .buttonStyle(.borderedProminent)
        .tint(Theme.accent)
      Button("message_someone") { onBrowse(.people) }
    }
  }
}
