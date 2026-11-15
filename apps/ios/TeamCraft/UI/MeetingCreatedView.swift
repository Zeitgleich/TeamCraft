import SwiftUI
import UIKit

extension SavedMeeting: Identifiable {
  public var id: String { meetingId }
}

/// Right after scheduling (#1905), as desktop's "Your meeting is ready": the meeting's link to copy
/// or share, and joining it. The link carries the organizer's key fingerprint as this device pinned
/// it (`meetingLink`, e2ee.md §4.6); until the core made it, nothing is handed out.
struct MeetingCreatedView: View {
  @Environment(AppModel.self) private var model
  @Environment(\.dismiss) private var dismiss
  let account: Account
  let saved: SavedMeeting

  /// The link as the sheet has it: being made, made, or not to be had on this device.
  private enum LinkState: Equatable {
    case verifying
    case ready(String)
    case unverified
  }

  @State private var link = LinkState.verifying
  @State private var isCopied = false

  var body: some View {
    NavigationStack {
      Form {
        Section {
          VStack(alignment: .leading, spacing: 4) {
            Text(verbatim: saved.title).font(.app(.headline))
            Text(timeText).font(.app(.subheadline)).foregroundStyle(Theme.textMuted)
          }
          Text("meeting_created_hint")
        }
        Section {
          switch link {
          case .verifying:
            HStack {
              ProgressView()
              Text("meeting_link_verifying").foregroundStyle(Theme.textMuted)
            }
          case .unverified:
            Text("meeting_link_unverified").foregroundStyle(Theme.danger)
            Button("retry") { link = .verifying }
          case .ready(let text):
            Text(verbatim: text).font(.app(.footnote).monospaced()).textSelection(.enabled)
            Button {
              UIPasteboard.general.string = text
              isCopied = true
            } label: {
              Label(isCopied ? "meeting_link_copied" : "meeting_copy_link", systemImage: "doc.on.doc")
            }
            ShareLink(item: text) { Label("meeting_share_link", systemImage: "square.and.arrow.up") }
          }
        } header: {
          Text("meeting_link")
        } footer: {
          Text("meeting_link_hint")
        }
        Section {
          Button {
            dismiss()
            model.joinMeeting(joinable, userId: account.userId)
          } label: {
            HStack {
              Spacer()
              Text("meeting_join").bold()
              Spacer()
            }
          }
        }
      }
      .navigationTitle("meeting_created_title")
      .navigationBarTitleDisplayMode(.inline)
      .toolbar {
        ToolbarItem(placement: .cancellationAction) {
          Button("done") { dismiss() }
        }
      }
    }
    .task(id: link == .verifying) {
      guard link == .verifying else { return }
      do {
        link = .ready(try await model.meetingLink(meetingId: saved.meetingId))
      } catch {
        link = .unverified
      }
    }
  }

  /// The saved meeting as the list has it, to join: the user scheduled it, so they organize it.
  private var joinable: Meeting {
    Meeting(
      id: saved.meetingId, title: saved.title, startsAt: saved.startsAtMs, endsAt: saved.endsAtMs,
      status: "scheduled", conversationId: saved.conversationId, spaceId: nil, attendees: nil,
      organizerUserId: account.userId)
  }

  private var timeText: String {
    let start = Date(timeIntervalSince1970: TimeInterval(saved.startsAtMs) / 1000)
    let end = Date(timeIntervalSince1970: TimeInterval(saved.endsAtMs) / 1000)
    return "\(start.formatted(date: .abbreviated, time: .shortened)) – \(end.formatted(date: .omitted, time: .shortened))"
  }
}
