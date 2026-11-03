import Foundation

/// Links the system hands to the app (docs/app-links.md). The core decides what a link means
/// (`openAppLink`); the link itself is never logged, since it can hold a key.
extension AppModel {
  /// A link arrived (Universal Link or `teamcraft://`). It waits until it can be acted on.
  func receiveAppLink(_ url: URL) {
    pendingAppLink = url.absoluteString
    isHostQuestionHandled = false
    Task { await actOnAppLink() }
  }

  /// Takes the waiting link as far as it can go now: the question at once, the place once signed in.
  func actOnAppLink() async {
    guard let link = pendingAppLink else { return }
    if case .starting = phase { return }
    guard
      let opened = openAppLink(
        link: link, maybeActiveOrigin: currentServerOrigin, isServerLocked: isServerLocked)
    else {
      pendingAppLink = nil
      return
    }
    appLinkWebURL = opened.maybeWebUrl.flatMap(URL.init(string:))
    switch opened.step {
    case .navigate:
      appLinkQuestion = nil
      guard case .ready = phase else { return }
      pendingAppLink = nil
      await show(opened.place)
    case .askForeignServer, .askWebOnly:
      if !isHostQuestionHandled { appLinkQuestion = opened }
    }
  }

  private func show(_ place: AppLinkPlace) async {
    switch place {
    case .home, .none:
      break
    case .conversation(let conversationId):
      openConversationId = conversationId
    case .permalink(let link):
      guard let target = await resolvePermalink(link) else {
        // Not on this device, or not the user's to read: the browser can say why.
        isPermalinkFromLinkMissing = true
        return
      }
      focusMessageId = target.message
      openConversationId = target.conversation
    case .meeting:
      isAskingAboutMeetingLink = true
    }
  }

  /// "Sign in there": the usual check and confirmation of a server the user typed.
  func signInForAppLink(host: String) {
    appLinkQuestion = nil
    isHostQuestionHandled = true
    Task { await checkServer(host) }
  }

  func openAppLinkInBrowser() {
    appLinkQuestion = nil
    pendingAppLink = nil
    isAskingAboutMeetingLink = false
    browserLink = appLinkWebURL.map(BrowserLink.init(url:))
  }

  func dropAppLink() {
    appLinkQuestion = nil
    pendingAppLink = nil
    isAskingAboutMeetingLink = false
  }

  func showMeetingsForAppLink() {
    isAskingAboutMeetingLink = false
    isShowingMeetingsFromLink = true
  }
}
