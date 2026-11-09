import SwiftUI

/// A channel's members (issue #1262), as the members section on desktop and web: everybody with
/// their role, the line for a channel where this device and a member's see different roles, and for
/// a workspace admin the look at the roles (e2ee.md §4.6.1). That look asks nothing of them: it is
/// here when they open the members. What they confirm is the list as it stood when they opened it.
struct ChannelMembersView: View {
  @Environment(AppModel.self) private var model
  @Environment(\.dismiss) private var dismiss
  let conversationId: String

  @State private var maybeInfo: ChannelMembersInfo?
  /// The list and the log as they stood when the admin opened the look: that's what they confirm.
  @State private var maybeShown: ChannelRolesReviewItem?
  @State private var isSigning = false
  @State private var maybeMessageKey: String?
  /// "Add people", where the server says the user may (#1534).
  @State private var canAdd = false
  @State private var isAdding = false

  var body: some View {
    NavigationStack {
      List {
        if canAdd {
          Section {
            Button("members_add", systemImage: "person.badge.plus") { isAdding = true }
          }
        }
        if let info = maybeInfo {
          if info.hasRoleAlarm {
            Section { hint("channel_roles_alarm") }
          }
          if let review = info.maybeReview {
            Section {
              reviewContent(live: review)
            } header: {
              Text("channel_roles_review_title")
            }
          }
          Section {
            ForEach(info.members, id: \.userId) { member in
              VStack(alignment: .leading, spacing: 2) {
                HStack {
                  Text(verbatim: member.name).foregroundStyle(Theme.text).lineLimit(1)
                  Spacer()
                  if let key = ChannelMembers.roleKey(member.role) { badge(key) }
                }
                // Only where the verified role log says who appointed the role (issue #1246).
                if let appointer = member.maybeAppointer {
                  Text(verbatim: ChannelMembers.appointedByLine(appointer))
                    .font(.app(.footnote)).foregroundStyle(Theme.textMuted)
                }
              }
            }
          }
        } else {
          ProgressView().frame(maxWidth: .infinity)
        }
      }
      .navigationTitle(Text("members_title"))
      .navigationBarTitleDisplayMode(.inline)
      .toolbar { ToolbarItem(placement: .confirmationAction) { Button("done") { dismiss() } } }
    }
    // The members and the signed roles are read afresh while the section is open.
    .task(id: conversationId) { await model.refreshChannelMembers(conversationId) }
    .task(id: model.changeCount) {
      maybeInfo = await model.channelMembers(conversationId) ?? maybeInfo
      canAdd = await model.browseSnapshot()?.conversations?.first { $0.id == conversationId }?.canAddMembers ?? canAdd
    }
    .sheet(isPresented: $isAdding) {
      AddChannelMembersView(conversationId: conversationId, memberIds: maybeInfo?.members.map(\.userId) ?? [])
    }
  }

  @ViewBuilder
  private func reviewContent(live: ChannelRolesReviewItem) -> some View {
    hint(live.needsALook ? "channel_roles_review_hint" : "channel_roles_review_hint_quiet")
    if let shown = maybeShown {
      ForEach(shown.rows, id: \.userId) { row in reviewRow(row) }
      if channelRolesReviewHasNew(review: shown) { hint("channel_roles_review_new_hint") }
      if shown.knowsNoRoles { hint("channel_roles_review_knows_none") }
      if shown.rows.contains(where: { $0.keyMark == .changed }) { hint("channel_roles_review_key_changed_hint") }
      if !shown.dropped.isEmpty {
        let names = ChannelMembers.droppedNames(
          shown.dropped, unknown: String(localized: "channel_roles_review_unknown"))
        Text(String(format: String(localized: "channel_roles_review_dropped"), names))
          .font(.app(.footnote)).foregroundStyle(Theme.textMuted)
      }
      if let refusal = shown.maybeRefusal { status(ChannelMembers.refusalKey(refusal)) }
      let hasChanged = channelRolesReviewHasChanged(shown: shown, maybeLive: live)
      if hasChanged { status("channel_roles_review_changed") }
      if let key = maybeMessageKey { status(key) }
      Button("channel_roles_review_confirm") { confirm(shown) }
        .buttonStyle(.borderedProminent)
        .disabled(isSigning || shown.maybeRefusal != nil || hasChanged)
      if hasChanged {
        Button("channel_roles_review_action") { open(live) }
      }
      Button("cancel", role: .cancel) { maybeShown = nil }
    } else {
      Button("channel_roles_review_action") { open(live) }
      if let key = maybeMessageKey { status(key) }
    }
  }

  private func reviewRow(_ row: RoleReviewRowItem) -> some View {
    HStack(spacing: 6) {
      if let name = row.maybeName {
        Text(verbatim: name).foregroundStyle(Theme.text).lineLimit(1)
      } else {
        Text("channel_roles_review_unknown").foregroundStyle(Theme.textMuted).lineLimit(1)
      }
      Spacer()
      if row.isNew { badge("channel_roles_review_new", isWarning: true) }
      if let key = ChannelMembers.keyMarkKey(row.keyMark) { badge(key, isWarning: true) }
      if let key = ChannelMembers.roleKey(row.role) { badge(key) }
    }
  }

  private func open(_ live: ChannelRolesReviewItem) {
    maybeMessageKey = nil
    maybeShown = live
  }

  private func confirm(_ shown: ChannelRolesReviewItem) {
    isSigning = true
    Task {
      let outcome = await model.signReviewedChannelRoles(conversationId, review: shown)
      isSigning = false
      maybeMessageKey = ChannelMembers.outcomeKey(outcome)
      // Done, or nothing signed: the next look starts from the list as it is now.
      maybeShown = nil
    }
  }

  private func hint(_ key: String) -> some View {
    Text(LocalizedStringKey(key)).font(.app(.footnote)).foregroundStyle(Theme.textMuted)
  }

  private func status(_ key: String) -> some View {
    hint(key).accessibilityAddTraits(.updatesFrequently)
  }

  private func badge(_ key: String, isWarning: Bool = false) -> some View {
    Text(LocalizedStringKey(key))
      .font(.app(.caption2).weight(.medium))
      .padding(.horizontal, 6)
      .padding(.vertical, 2)
      .foregroundStyle(isWarning ? Theme.mentionText : Theme.accentStrong)
      .background(isWarning ? Theme.mentionBackground : Theme.accentSoft, in: Capsule())
  }
}
