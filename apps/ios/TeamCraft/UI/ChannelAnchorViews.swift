import SwiftUI

/// The server offered this device a group the user's devices didn't pin for this channel (e2ee.md
/// §4.6.5, #1842): it can't be read here, and new messages stay unreadable. As on desktop and web,
/// a calm notice at the top of the channel that says whom to tell; nothing to tap.
struct ChannelUnreachableBanner: View {
  @Environment(AppModel.self) private var model
  let conversationId: String

  @State private var isUnreachable = false

  var body: some View {
    VStack(spacing: 0) {
      if isUnreachable {
        Label {
          Text("anchor_refused")
        } icon: {
          Image(systemName: "exclamationmark.lock.fill")
        }
        .font(.app(.footnote))
        .foregroundStyle(Theme.danger)
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal)
        .padding(.vertical, 8)
        .background(Theme.surface)
        .accessibilityElement(children: .combine)
      }
    }
    .task(id: "\(conversationId)/\(model.changeCount)") {
      isUnreachable = await model.isChannelUnreachable(conversationId)
    }
  }
}
