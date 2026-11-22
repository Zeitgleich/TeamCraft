import SwiftUI
import UIKit
import UniformTypeIdentifiers

/// How long a copied one-time link stays on the pasteboard: it is a credential.
private let guestLinkPasteboardLifetime: TimeInterval = 120

/// "Invite guests" of a meeting the user organizes or co-hosts (#1905), as desktop's: a personal
/// one-time link for one guest, the organizer's code guests may compare, and for the organizer a
/// new meeting link that ends the current one. Every link carries the organizer's key fingerprint
/// as this device pinned it (e2ee.md §4.6); without it the core makes none.
struct MeetingGuestsView: View {
  @Environment(AppModel.self) private var model
  @Environment(\.dismiss) private var dismiss
  let meeting: Meeting
  let canRenewLink: Bool
  /// The meeting link changed: the list reads it again.
  let onRenewed: () async -> Void

  /// The one-time link as the sheet has it.
  private enum GuestLink: Equatable {
    case none, making, failed
    case made(String)
  }

  @State private var guestLink = GuestLink.none
  @State private var isCopied = false
  @State private var maybeCode: String?
  @State private var isConfirmingRenew = false
  @State private var isRenewing = false
  @State private var maybeRenewNote: RenewNote?

  private enum RenewNote: Equatable {
    case renewed
    case failed(AppFailure)
  }

  var body: some View {
    NavigationStack {
      Form {
        Section {
          Text(verbatim: meeting.title).font(.app(.headline))
        }
        guestLinkSection
        if let code = maybeCode {
          Section {
            Text(verbatim: code).font(.app(.title3).monospaced())
              .accessibilityLabel(Text(verbatim: code.map(String.init).joined(separator: " ")))
          } header: {
            Text("meeting_organizer_code")
          } footer: {
            Text("meeting_organizer_code_hint")
          }
        }
        if canRenewLink {
          renewSection
        }
      }
      .navigationTitle("meeting_invite_guests")
      .navigationBarTitleDisplayMode(.inline)
      .toolbar {
        ToolbarItem(placement: .confirmationAction) {
          Button("done") { dismiss() }
        }
      }
      .confirmationDialog(
        "meeting_new_link_confirm_title", isPresented: $isConfirmingRenew, titleVisibility: .visible
      ) {
        Button("meeting_new_link_confirm", role: .destructive) { renew() }
        Button("meeting_new_link_keep", role: .cancel) {}
      } message: {
        Text("meeting_new_link_hint")
      }
    }
    // The code shows once the core pinned the organizer's key; offline it stays away.
    .task(id: meeting.id) { maybeCode = await model.meetingOrganizerCode(meetingId: meeting.id) }
  }

  private var guestLinkSection: some View {
    Section {
      switch guestLink {
      case .none, .making, .failed:
        Button(action: makeLink) {
          HStack {
            Label("meeting_guest_link_make", systemImage: "person.badge.plus")
            if guestLink == .making {
              Spacer()
              ProgressView()
            }
          }
        }
        .disabled(guestLink == .making)
        if guestLink == .failed {
          Text("meeting_guest_link_failed").foregroundStyle(Theme.danger)
        }
      case .made(let link):
        Text(verbatim: link).font(.app(.footnote).monospaced()).textSelection(.enabled)
        Button {
          copy(link)
        } label: {
          Label(isCopied ? "meeting_link_copied" : "meeting_copy_link", systemImage: "doc.on.doc")
        }
        ShareLink(item: link) { Label("meeting_share_link", systemImage: "square.and.arrow.up") }
      }
    } header: {
      Text("meeting_guest_link_label")
    } footer: {
      Text("meeting_guest_link_hint")
    }
  }

  private var renewSection: some View {
    Section {
      Button(role: .destructive) {
        isConfirmingRenew = true
      } label: {
        HStack {
          Label("meeting_new_link", systemImage: "arrow.triangle.2.circlepath")
          if isRenewing {
            Spacer()
            ProgressView()
          }
        }
      }
      .disabled(isRenewing)
      switch maybeRenewNote {
      case .renewed: Text("meeting_link_renewed")
      case .failed(let failure): Text(failure.message).foregroundStyle(Theme.danger)
      case nil: EmptyView()
      }
    } footer: {
      Text("meeting_new_link_hint")
    }
  }

  private func makeLink() {
    guard guestLink != .making else { return }
    guestLink = .making
    Task {
      do {
        guestLink = .made(try await model.meetingGuestLink(meetingId: meeting.id).link)
      } catch {
        guestLink = .failed
      }
    }
  }

  /// A one-time link is a credential: kept off other devices and taken off the pasteboard later.
  private func copy(_ link: String) {
    UIPasteboard.general.setItems(
      [[UTType.plainText.identifier: link]],
      options: [.localOnly: true, .expirationDate: Date().addingTimeInterval(guestLinkPasteboardLifetime)])
    isCopied = true
  }

  private func renew() {
    guard !isRenewing else { return }
    isRenewing = true
    maybeRenewNote = nil
    Task {
      defer { isRenewing = false }
      do {
        try await model.renewMeetingLink(meetingId: meeting.id)
        maybeRenewNote = .renewed
        await onRenewed()
      } catch {
        maybeRenewNote = .failed(AppFailure(CoreFailure(error)))
      }
    }
  }
}
