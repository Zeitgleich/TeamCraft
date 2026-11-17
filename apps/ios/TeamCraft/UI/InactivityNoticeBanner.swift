import SwiftUI

/// What the owners of a quiet channel are told in the channel (issue #212): nobody wrote in it for a
/// while, when it is archived or deleted, and "keep open". The core decides what is said and to whom
/// (`ConversationItem.maybeInactivityNotice` is nil for everybody else); this only draws it.
struct InactivityNoticeBanner: View {
  @Environment(AppModel.self) private var model
  let conversationId: String
  let notice: InactivityNoticeItem
  @State private var isPending = false
  @State private var hasFailed = false

  var body: some View {
    VStack(alignment: .leading, spacing: 8) {
      text
        .font(.app(.footnote))
        .frame(maxWidth: .infinity, alignment: .leading)
      if notice.offersKeepOpen {
        Button {
          Task { await keepOpen() }
        } label: {
          Text(notice.kind == .archivedByPolicy ? "inactivity_notice_postpone" : "inactivity_notice_keep")
        }
        .buttonStyle(.bordered)
        .controlSize(.small)
        .disabled(isPending)
      }
    }
    .padding(.horizontal, 16)
    .padding(.vertical, 8)
    .background(.bar)
    .accessibilityElement(children: .contain)
    .alert("inactivity_notice_failed", isPresented: $hasFailed) {
      Button("cancel", role: .cancel) {}
    }
  }

  @ViewBuilder
  private var text: some View {
    let date = notice.maybeAtMs.map {
      Date(timeIntervalSince1970: Double($0) / 1000).formatted(date: .abbreviated, time: .omitted)
    }
    switch notice.kind {
    case .quiet:
      if let date { Text("inactivity_notice_archive_on \(date)") } else { Text("inactivity_notice_quiet") }
    case .archivedByPolicy:
      if let date { Text("inactivity_notice_delete_on \(date)") } else { Text("inactivity_notice_archived") }
    case .keptOpen:
      Text("inactivity_notice_kept_open \(date ?? "")")
    }
  }

  private func keepOpen() async {
    isPending = true
    let maybeFailure = await model.performReporting(.snoozeChannelInactivity(conversationId: conversationId))
    isPending = false
    hasFailed = maybeFailure != nil
  }
}
