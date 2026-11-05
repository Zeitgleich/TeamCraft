import Foundation

/// A share on its way from the picker into a conversation's composer.
struct ShareHandover: Equatable {
  let conversationId: String
  let share: StagedShare
}

/// What other apps shared through the share extension (issue #208): it waits in the App Group, also
/// through a sign-in and the app lock, until the user picked a conversation.
extension AppModel {
  /// Looks for a share; runs when the app comes to the front and when the account changes.
  func loadWaitingShare() {
    guard let inbox = ShareInbox.shared() else { return }
    inbox.prune()
    var handedOver = Set<String>()
    if let handover = shareHandover { handedOver.insert(handover.share.id) }
    waitingShare = inbox.oldest(except: handedOver)
  }

  /// Where a share can go: conversations the user may post in, each with its last activity.
  func shareTargets() async -> [ConversationItem] {
    (try? await core().shareTargets()) ?? []
  }

  /// The picker's choice: the conversation opens with its composer filled in.
  func sendShare(_ share: StagedShare, to conversationId: String) {
    shareHandover = ShareHandover(conversationId: conversationId, share: share)
    waitingShare = nil
    openConversationId = conversationId
  }

  func discardShare(_ share: StagedShare) {
    ShareInbox.shared()?.delete(share.id)
    loadWaitingShare()
  }

  /// The composer of `conversationId` takes the share over, once. Files that can't be attached (over
  /// the upload limit, gone) come back as errors for the composer to show.
  func takeShare(for conversationId: String) async -> (text: String, files: [PickedFile], error: AttachmentError?)? {
    guard let handover = shareHandover, handover.conversationId == conversationId,
      let inbox = ShareInbox.shared()
    else { return nil }
    shareHandover = nil
    let importer = await attachmentImporter()
    var files: [PickedFile] = []
    var maybeError: AttachmentError?
    for file in handover.share.files {
      do {
        files.append(try importer.importPickedMedia(PickedMedia(url: inbox.url(of: file, in: handover.share))))
      } catch let error as AttachmentError {
        maybeError = error
      } catch {
        maybeError = .unreadable
      }
    }
    inbox.delete(handover.share.id)
    loadWaitingShare()
    return (handover.share.text, files, maybeError)
  }
}
