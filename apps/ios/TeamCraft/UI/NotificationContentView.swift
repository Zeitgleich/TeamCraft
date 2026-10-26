import SwiftUI

/// How much message notifications show on this device (issue #331): sender and message, the sender
/// only, or nothing but "New message". The core stores the choice where the notification extension
/// reads it and applies it with the workspace policy as a limit (the stricter wins), so the lock
/// screen never shows more than either allows.
struct NotificationContentView: View {
  @Environment(AppModel.self) private var model

  @State private var content = NotificationContent.content
  @State private var maybeWorkspaceLimit: NotificationContent?
  @State private var isLoaded = false
  @State private var isFailed = false

  var body: some View {
    Form {
      Section {
        Picker("notification_content_title", selection: $content) {
          Text("notification_content_full").tag(NotificationContent.content)
          Text("notification_content_sender_only").tag(NotificationContent.senderOnly)
          Text("notification_content_nothing").tag(NotificationContent.nothing)
        }
        .pickerStyle(.inline)
        .labelsHidden()
        .disabled(!isLoaded)
      } footer: {
        VStack(alignment: .leading, spacing: 4) {
          Text("notification_content_hint")
          if let limit = maybeWorkspaceLimit {
            Text("notification_content_limit \(Self.label(for: limit))")
          }
          if isFailed {
            Text("error_generic").foregroundStyle(Theme.danger)
          }
        }
      }
    }
    .navigationTitle("notification_content_title")
    .navigationBarTitleDisplayMode(.inline)
    .task { await load() }
    .onChange(of: content) { _, chosen in
      guard isLoaded else { return }
      Task { await save(chosen) }
    }
  }

  private func load() async {
    do {
      let settings = try await model.core().notificationContent()
      content = settings.device
      // Only a real limit is worth a hint.
      maybeWorkspaceLimit = settings.workspace == .content ? nil : settings.workspace
      isFailed = false
    } catch {
      isFailed = true
    }
    isLoaded = !isFailed
  }

  private func save(_ chosen: NotificationContent) async {
    do {
      try await model.core().setNotificationContent(content: chosen)
      isFailed = false
    } catch {
      isFailed = true
    }
  }

  private static func label(for content: NotificationContent) -> String {
    switch content {
    case .content: String(localized: "notification_content_full")
    case .senderOnly: String(localized: "notification_content_sender_only")
    case .nothing: String(localized: "notification_content_nothing")
    }
  }
}
