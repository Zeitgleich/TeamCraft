import SwiftUI

/// Guest memberships ending soon, on top of the home list (issue #212): the guest's own, and for the
/// sponsor and the space's admins the ones they can extend.
struct GuestNoticeSection: View {
  @Environment(AppModel.self) private var model

  var body: some View {
    if !model.expiringGuests.isEmpty {
      Section {
        ForEach(model.expiringGuests) { guest in
          row(guest)
        }
      } header: {
        Text("guest_notice_title")
      }
    }
  }

  @ViewBuilder
  private func row(_ guest: ExpiringGuest) -> some View {
    let date = guest.expiresAtDate.formatted(date: .abbreviated, time: .omitted)
    if guest.isOwn {
      Text("guest_notice_self \(guest.spaceName) \(date)")
    } else {
      VStack(alignment: .leading, spacing: 6) {
        Text("guest_notice_row \(guest.user.displayName) \(guest.spaceName) \(date)")
        HStack {
          ForEach(guest.extendOptions, id: \.self) { days in
            Button("guest_notice_extend \(days)") {
              let expiresAtMs = guest.expiresAt + days * ExpiringGuest.dayMs
              Task {
                await model.perform(.extendGuest(spaceId: guest.spaceId, userId: guest.user.id, expiresAtMs: expiresAtMs))
              }
            }
            .buttonStyle(.bordered)
          }
        }
      }
    }
  }
}
