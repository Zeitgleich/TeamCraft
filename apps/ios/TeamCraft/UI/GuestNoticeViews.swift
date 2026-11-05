import SwiftUI

/// Guest memberships ending soon, on top of the home list (issue #212): the guest's own, and for the
/// sponsor and the space's admins the ones they can extend.
struct GuestNoticeSection: View {
  @Environment(AppModel.self) private var model
  @State private var pendingId: String?
  @State private var hasFailed = false

  var body: some View {
    if !model.expiringGuests.isEmpty {
      Section {
        ForEach(model.expiringGuests) { guest in
          row(guest)
        }
      } header: {
        Text("guest_notice_title")
      }
      .alert("guest_notice_failed", isPresented: $hasFailed) {
        Button("cancel", role: .cancel) {}
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
          ForEach(guest.extendOffers ?? [], id: \.self) { offer in
            Button {
              Task { await extend(guest, to: offer) }
            } label: {
              if let days = offer.days {
                Text("guest_notice_extend \(days)")
              } else {
                Text("guest_notice_extend_until \(offer.untilDate.formatted(date: .abbreviated, time: .omitted))")
              }
            }
            .buttonStyle(.bordered)
            .disabled(pendingId == guest.id)
          }
        }
      }
    }
  }

  private func extend(_ guest: ExpiringGuest, to offer: ExpiringGuest.Offer) async {
    pendingId = guest.id
    let maybeFailure = await model.performReporting(
      .extendGuest(spaceId: guest.spaceId, userId: guest.user.id, expiresAtMs: offer.until))
    pendingId = nil
    hasFailed = maybeFailure != nil
  }
}
