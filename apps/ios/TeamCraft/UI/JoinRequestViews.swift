import SwiftUI

/// Admins' requests to join channels the user owns, on top of the Spaces list (admin.md §2.4.1):
/// each with what approving shares, or why only Decline is offered.
struct JoinRequestSection: View {
  @Environment(AppModel.self) private var model

  var body: some View {
    if !model.joinRequests.isEmpty {
      Section {
        ForEach(model.joinRequests, id: \.notificationTag) { request in
          JoinRequestCard(request: request, isInChannel: false)
        }
      } header: {
        Text("join_requests_title")
      }
    }
  }
}

/// The requests for the open channel, above its messages: the owner decides where they are.
struct JoinRequestBanner: View {
  @Environment(AppModel.self) private var model
  let conversationId: String

  var body: some View {
    let requests = JoinRequests.requests(in: conversationId, of: model.joinRequests)
    if !requests.isEmpty {
      VStack(spacing: 8) {
        ForEach(requests, id: \.notificationTag) { request in
          JoinRequestCard(request: request, isInChannel: true)
            .padding()
            .background(Theme.surface)
            .clipShape(RoundedRectangle(cornerRadius: Theme.radius, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: Theme.radius, style: .continuous).stroke(Theme.border))
        }
      }
      .padding(.horizontal)
      .padding(.vertical, 6)
    }
  }
}

/// One request, naming the admin with their username (two people may share a name). Approve is the
/// prominent action and only there for an admin of the fresh verified roster; it shares what a
/// self-join gets, the history from the join on, and the earlier history only when the owner turns
/// it on. Decline is plain (the admin can ask again later). While a decision runs, everything
/// waits. `isInChannel`: shown in the channel itself, which needs no channel name.
struct JoinRequestCard: View {
  @Environment(AppModel.self) private var model
  let request: JoinRequestItem
  let isInChannel: Bool

  @State private var sharesHistory = false

  var body: some View {
    let isPending = model.pendingJoinKeys.contains(JoinRequests.key(request))
    VStack(alignment: .leading, spacing: 8) {
      Text(summary).font(.body).foregroundStyle(Theme.text)
      Text(note).font(.footnote).foregroundStyle(Theme.textMuted)
      if isApprovable {
        Toggle("join_request_share_history", isOn: $sharesHistory)
          .font(.subheadline)
          .tint(Theme.accent)
          .disabled(isPending)
      }
      HStack(spacing: 12) {
        Spacer()
        if isPending { ProgressView().controlSize(.small) }
        Button("join_request_decline") { Task { await model.decline(request) } }
          .buttonStyle(.bordered)
        if isApprovable {
          Button("join_request_approve") {
            let history: JoinHistory = sharesHistory ? .full : .fromJoin
            Task { await model.approve(request, history: history) }
          }
            .buttonStyle(.borderedProminent)
            .tint(Theme.accent)
        }
      }
      .disabled(isPending)
    }
    .padding(.vertical, isInChannel ? 0 : 4)
  }

  private var isApprovable: Bool { request.standing == .approvable }

  private var summary: String {
    if isInChannel || request.conversationName.isEmpty {
      return String(localized: "join_request_summary_here \(request.userLabel)")
    }
    let channel = "#" + request.conversationName
    return String(localized: "join_request_summary \(request.userLabel) \(channel)")
  }

  private var note: LocalizedStringKey {
    if !isApprovable { return "join_request_not_admin" }
    return sharesHistory ? "join_request_hint_history" : "join_request_hint"
  }
}

extension View {
  /// Shows why the last action on a join request didn't go through, once. One view presents it at
  /// a time: the chat list, unless a sheet that has its own (the channel browser) is in front.
  func joinNoteAlert(_ model: AppModel, isActive: Bool = true) -> some View {
    alert(
      "join_request_note_title",
      isPresented: Binding(
        get: { isActive && model.joinNote != nil }, set: { if !$0 { model.joinNote = nil } }),
      presenting: model.joinNote
    ) { _ in
      Button("done", role: .cancel) {}
    } message: { note in
      Text(note.message)
    }
  }
}
