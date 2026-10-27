import Foundation
import UserNotifications

/// Admins asking to join a private channel under `admin_self_join = owner_approval` (admin.md
/// §2.4.1, issue #799): the owner's list and decisions, the admin's own request, and the local
/// notifications while the app runs. No push wakes the phone for a request yet; the core reads the
/// list again after every connection, so a request that came in meanwhile shows then.
extension AppModel {
  // MARK: Reading

  /// Reads the requests waiting for the user's decision and notifies about new ones; called on
  /// every change the core reports (both reads are cheap).
  func refreshJoinRequests() async {
    guard case .ready = phase, let core = try? core() else {
      if !joinRequests.isEmpty { joinRequests = [] }
      return
    }
    // A failed read keeps what is shown; the next change reads again.
    if let requests = try? await core.joinRequests(), requests != joinRequests {
      joinRequests = requests
    }
    await notifyJoinRequests(core)
  }

  // MARK: Acting

  /// Lets the admin in, sharing `history`: the earlier history only when the owner ticked it.
  func approve(_ request: JoinRequestItem, history: JoinHistory) async {
    await runJoin(
      .approve, key: JoinRequests.key(request), conversationId: request.conversationId, userId: request.userId,
      history: history)
  }

  func decline(_ request: JoinRequestItem) async {
    await runJoin(
      .decline, key: JoinRequests.key(request), conversationId: request.conversationId, userId: request.userId)
  }

  /// "Ask to join" in the channel browser.
  func askToJoin(conversationId: String) async {
    await runJoin(.ask, key: conversationId, conversationId: conversationId, userId: "")
  }

  func withdrawJoinRequest(conversationId: String) async {
    await runJoin(.withdraw, key: conversationId, conversationId: conversationId, userId: "")
  }

  /// Approve or Decline in a request's notification (in the background, after unlocking). Only for
  /// the app's own notification of a request that is still open; a decision that doesn't go
  /// through shows the notification again with why, so it never fails silently. Approve here never
  /// shares the earlier history (the notification says so); that choice is in the app.
  func handle(joinRequestAction identifier: String, tag: String?, notificationId: String) async {
    let step: JoinStep
    switch identifier {
    case NotificationCategories.approveJoinAction: step = .approve
    case NotificationCategories.declineJoinAction: step = .decline
    default: return
    }
    let center = UNUserNotificationCenter.current()
    guard NotificationAuthenticator().isValid(tag: tag, for: notificationId),
      let core = try? core(),
      let request = try? await core.joinRequests().first(where: { $0.notificationTag == notificationId })
    else {
      // Not ours, or the request is gone: nothing to decide.
      center.removeDeliveredNotifications(withIdentifiers: [notificationId])
      return
    }
    let maybeNote: JoinNote?
    do {
      let outcome = try await step.run(
        on: core, conversationId: request.conversationId, userId: request.userId, history: .fromJoin)
      maybeNote = JoinRequests.note(after: step, outcome: outcome)
    } catch {
      maybeNote = .failed(AppFailure(CoreFailure(error)))
    }
    guard let note = maybeNote, note != .gone else {
      center.removeDeliveredNotifications(withIdentifiers: [notificationId])
      return
    }
    let notice = JoinRequestNotice(request, showsNames: await joinRequestShowsNames(core), maybeNote: note)
    try? await center.add(notice.request(for: request))
  }

  // MARK: Private

  private func runJoin(
    _ step: JoinStep, key: String, conversationId: String, userId: String, history: JoinHistory = .fromJoin
  ) async {
    guard !pendingJoinKeys.contains(key) else { return }
    pendingJoinKeys.insert(key)
    defer { pendingJoinKeys.remove(key) }
    do {
      let outcome = try await step.run(on: core(), conversationId: conversationId, userId: userId, history: history)
      joinNote = JoinRequests.note(after: step, outcome: outcome)
    } catch {
      joinNote = .failed(AppFailure(CoreFailure(error)))
    }
  }

  /// One notification per new request, once per device (the core remembers which); the ones whose
  /// request is gone are withdrawn.
  private func notifyJoinRequests(_ core: TeamCraft) async {
    guard let changes = try? await core.joinRequestAlerts() else { return }
    let center = UNUserNotificationCenter.current()
    if !changes.goneTags.isEmpty {
      center.removeDeliveredNotifications(withIdentifiers: changes.goneTags)
      center.removePendingNotificationRequests(withIdentifiers: changes.goneTags)
    }
    guard !changes.new.isEmpty else { return }
    let showsNames = await joinRequestShowsNames(core)
    for request in changes.new {
      try? await center.add(JoinRequestNotice(request, showsNames: showsNames).request(for: request))
    }
  }

  /// Names (and so the actions) only where messages would show them: not behind the app lock, and
  /// not while this device or the workspace policy shows nothing in notifications.
  private func joinRequestShowsNames(_ core: TeamCraft) async -> Bool {
    guard !SharedContainer().isAppLockOn, let settings = try? await core.notificationContent() else { return false }
    return settings.device != .nothing && settings.workspace != .nothing
  }
}
