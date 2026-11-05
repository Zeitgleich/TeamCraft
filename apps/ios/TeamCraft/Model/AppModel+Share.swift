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

  /// Somebody signed in: a share kept through a session that ran out is only for the same person.
  func claimWaitingShare(for userId: String) {
    guard let inbox = ShareInbox.shared() else { return }
    // Runs again for the same person right after a sign-in, which must not take the notice back.
    if inbox.claim(for: userId) {
      isShareGoneForAccount = true
      // The folder of one on its way into a composer went too.
      shareHandover = nil
    }
    loadWaitingShare()
  }

  /// Where a share can go: conversations the user may post in, each with its last activity.
  func shareTargets() async -> [ConversationItem] {
    (try? await core().shareTargets()) ?? []
  }

  /// The picker's choice: the conversation opens with its composer filled in.
  func sendShare(_ share: StagedShare, to conversationId: String) {
    shareImportError = nil
    shareHandover = ShareHandover(conversationId: conversationId, share: share)
    waitingShare = nil
    openConversationId = conversationId
  }

  func discardShare(_ share: StagedShare) {
    shareImportError = nil
    ShareInbox.shared()?.delete(share.id)
    loadWaitingShare()
  }

  /// The composer of `conversationId` takes the share over, once. Files that can't be attached (over
  /// the upload limit, gone) stay in the inbox and the picker comes back with them and says why: it
  /// opens at this moment, so an alert of the composer's would be refused. The handover ends last: `ChatPane`'s task is keyed by it, and ending it
  /// first would cancel the task that is taking the share.
  func takeShare(for conversationId: String) async -> (text: String, files: [PickedFile])? {
    guard !isTakingShare, let handover = shareHandover, handover.conversationId == conversationId,
      let inbox = ShareInbox.shared()
    else { return nil }
    isTakingShare = true
    defer { isTakingShare = false }
    let importer = await attachmentImporter()
    var files: [PickedFile] = []
    var failed: [StagedShareFile] = []
    var maybeError: AttachmentError?
    for file in handover.share.files {
      do {
        files.append(try importer.importPickedMedia(PickedMedia(url: inbox.url(of: file, in: handover.share))))
      } catch let error as AttachmentError {
        failed.append(file)
        maybeError = error
      } catch {
        failed.append(file)
        maybeError = .unreadable
      }
    }
    if failed.isEmpty {
      inbox.delete(handover.share.id)
    } else {
      // The text goes to the composer now; only the files wait.
      var rest = handover.share
      rest.text = ""
      rest.files = failed
      rest.leftOut = 0
      try? inbox.commit(rest)
    }
    shareImportError = maybeError
    shareHandover = nil
    loadWaitingShare()
    return (handover.share.text, files)
  }
}
