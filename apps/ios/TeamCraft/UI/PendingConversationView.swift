import SwiftUI

/// A conversation opened from a notification before this device stored it (a channel new to the
/// device, a slow catch-up; issue #1065): the name and the message the notification showed, and a
/// quiet loading line. The list replaces it with the conversation once the sync brings it.
struct PendingConversationView: View {
  @Environment(AppModel.self) private var model
  let conversationId: String

  @State private var maybeNotified: NotifiedConversation?
  @State private var hasLoaded = false

  var body: some View {
    Group {
      // Nothing kept (a conversation left or removed meanwhile): as with no selection.
      if hasLoaded && maybeNotified == nil {
        Text("no_conversation_selected").foregroundStyle(Theme.textMuted)
      } else {
        pending
      }
    }
    .task(id: model.changeCount) {
      maybeNotified = await model.notifiedConversation(conversationId)
      hasLoaded = true
    }
  }

  private var pending: some View {
    VStack(alignment: .leading, spacing: 8) {
      Spacer()
      // A reply in a thread isn't part of the conversation's own feed.
      if let message = maybeNotified?.maybeMessage, message.maybeThreadId == nil,
        let text = message.maybeText ?? message.maybeFileName
      {
        VStack(alignment: .leading, spacing: 2) {
          if let sender = message.maybeSenderName {
            Text(verbatim: sender).font(.subheadline.weight(.semibold))
          }
          // The members' text, never a translation key.
          Text(verbatim: text)
        }
        .accessibilityElement(children: .combine)
      }
      HStack(spacing: 8) {
        ProgressView().controlSize(.small)
        Text("messages_loading").font(.caption).foregroundStyle(Theme.textMuted)
      }
    }
    .frame(maxWidth: .infinity, alignment: .leading)
    .padding()
    .background(Theme.background)
    .navigationBarTitleDisplayMode(.inline)
    .toolbar {
      ToolbarItem(placement: .principal) {
        VStack(spacing: 0) {
          if let name = maybeNotified?.maybeName {
            Text(verbatim: name).font(.headline).lineLimit(1)
          }
          Text("conversation_loading").font(.caption).foregroundStyle(Theme.textMuted).lineLimit(1)
        }
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(.isHeader)
      }
    }
  }
}
