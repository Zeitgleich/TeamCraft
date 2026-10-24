import Foundation
import Observation
import SwiftUI
import UIKit
import UserNotifications

/// Why something failed, as the screens show it (translated per case).
enum AppFailure: Equatable {
  case network, signedOut, wrongPassphrase, storage, signInFailed, recoveryPending, generic

  init(_ failure: CoreFailure) {
    switch failure {
    case .network: self = .network
    case .signedOut: self = .signedOut
    case .wrongPassphrase: self = .wrongPassphrase
    case .storage: self = .storage
    case .recoveryPending: self = .recoveryPending
    case .notEnrolled, .other: self = .generic
    }
  }

  var message: LocalizedStringKey {
    switch self {
    case .network: return "error_network"
    case .signedOut: return "error_signed_out"
    case .wrongPassphrase: return "error_wrong_passphrase"
    case .storage: return "error_storage"
    case .signInFailed: return "sign_in_failed"
    case .recoveryPending: return "recovery_pending"
    case .generic: return "error_generic"
    }
  }
}

struct Account: Equatable {
  let userId: String
  let displayName: String
  /// Shown for out-of-band comparison (safety number).
  let identityFingerprint: String
}

/// A device link this (new) device waits for: the code the other device scans or types.
struct LinkProgress: Equatable {
  let code: String
  var maybeShortCode: String?
}

/// An organization recovery this device asked for: the code the user reads to an admin.
struct RecoveryProgress: Equatable {
  let verificationCode: String
}

/// What the app shows at the top level.
enum AppPhase {
  case starting
  /// Asks for the server address; `address` prefills the field (a preconfigured or the last input).
  case needsServer(address: String, failure: ServerFailure?)
  /// A server passed the checks; the user confirms it before signing in.
  case confirmServer(CheckedServer)
  case signedOut(AppFailure?)
  case needsEnrollment(hasExistingIdentity: Bool, failure: AppFailure?)
  case linking(LinkProgress)
  case recovering(RecoveryProgress, failure: AppFailure?)
  case ready(Account)
  case failed(AppFailure)
}

/// The app's state and its one entry to the Rust core. Screens read; the core does the work
/// (docs/e2ee.md, ARCHITECTURE.md §8). All account data stays in the core's encrypted store.
@MainActor
@Observable
final class AppModel {
  static let shared = AppModel()

  private(set) var phase: AppPhase = .starting
  private(set) var isBusy = false
  /// Increments on every store change; screens re-read when it changes.
  private(set) var changeCount = 0
  /// The conversation a notification asked to open.
  var openConversationId: String?
  /// A message to show once its conversation is open (from a message link).
  var focusMessageId: String?
  /// The conversation on screen; its notifications are not shown as banners.
  var visibleConversationId: String?
  private(set) var pushStatus: PushStatus = .unknown
  /// Where the core's gateway connection stands (`nil`: no core open); the banner follows it.
  private(set) var connection: ConnectionState?

  let calls = CallManager()
  /// The server's brand for the confirmation and sign-in screens.
  let brand = BrandStore()
  /// The server the device's management or the build names (read at start).
  private(set) var managedServer = ManagedServer(maybeAddress: nil, isLocked: false)
  /// The account's server; `nil` until one is chosen.
  private(set) var maybeServer: StoredServer?
  /// What the sign-in screen offers; `nil` while it is being asked for.
  private(set) var maybeSignInPage: SignInPageInfo?

  @ObservationIgnored private let servers = ServerStore()
  @ObservationIgnored private lazy var vault = SessionVault(servers: servers)
  @ObservationIgnored private let container = SharedContainer()
  @ObservationIgnored private lazy var factory = CoreFactory(container: container, vault: vault, servers: servers)
  @ObservationIgnored private let login = WebLogin()
  @ObservationIgnored private let serverInfo = ServerInfoClient()
  @ObservationIgnored private let localAuth = LocalAuthClient()
  /// The sign-in page came with the server check just now: the screen need not ask again.
  @ObservationIgnored private var isSignInPageFromCheck = false
  @ObservationIgnored private lazy var processLock = ProcessLock(fileURL: container.lockFileURL)
  @ObservationIgnored private let storeChanges = StoreChangeNotifier()
  @ObservationIgnored private var maybeCore: TeamCraft?
  @ObservationIgnored private var isSyncing = false
  @ObservationIgnored private var maybePushToken: String?
  @ObservationIgnored private var maybeReconcile: Task<Void, Never>?

  private init() {}

  enum PushStatus: Equatable {
    case unknown, enabled, denied
  }

  // MARK: Start and sign-in

  /// Opens the stored server and session, if any. Safe to call more than once.
  func bootstrap() async {
    guard case .starting = phase else { return }
    storeChanges.observe { [weak self] in self?.storeDidChange() }
    // Starts following the network, so changes reach the core from now on.
    _ = NetworkMonitor.shared
    removeLegacyAccount()
    // Copies of picked files older than a week were uploaded long ago.
    container.pruneUploads(olderThan: 7 * 24 * 3600)
    await startWithServer()
  }

  /// After a failed start: tries to open the account again.
  func retry() async {
    phase = .starting
    await startWithServer()
  }

  // MARK: Server

  /// Whether the user may only use the server the device's management set.
  var isServerLocked: Bool { managedServer.isLocked }

  /// Checks what the user typed or pasted (address, URL, invite or message link): normalises it,
  /// asks the server and shows it for confirmation; stays at the entry with the reason otherwise.
  func checkServer(_ input: String) async {
    await busy {
      do {
        let address = try normalizeServerAddress(input: input, allowInsecureHttp: AppEnvironment.allowsInsecureHttp)
        guard managedServer.allows(origin: address.origin, normalize: Self.normalizedOrigin) else {
          phase = .needsServer(address: input, failure: .locked)
          return
        }
        guard let url = URL(string: address.serverInfoUrl) else { throw URLError(.badURL) }
        let answer = try await serverInfo.fetch(url)
        let environment = AppEnvironment.current
        let checked = try checkServerInfo(
          origin: address.origin, status: answer.status, contentType: answer.contentType, body: answer.body,
          clientVersion: environment.clientVersion, redirectUri: environment.oidcRedirectURI.absoluteString,
          allowInsecureHttp: AppEnvironment.allowsInsecureHttp, language: Locale.preferredLanguages.first ?? "en")
        brand.select(checked.server.address, initial: checked.maybeBrand)
        phase = .confirmServer(checked)
      } catch let error as URLError {
        phase = .needsServer(address: input, failure: ServerFailure(transport: error))
      } catch {
        phase = .needsServer(address: input, failure: ServerFailure(entryError: error))
      }
    }
  }

  /// The user confirmed the checked server: it becomes the account's server, sign-in follows.
  func confirmServer() {
    guard case .confirmServer(let checked) = phase else { return }
    servers.activate(checked.server)
    maybeServer = checked.server
    maybeSignInPage = checked.signInPage
    isSignInPageFromCheck = true
    phase = .signedOut(nil)
  }

  /// Back to the server entry, from the confirmation or the sign-in screen (signed out, so no
  /// account data is at stake). Not while the server is locked.
  func changeServer() {
    guard !managedServer.isLocked else { return }
    if case .signedOut = phase {
      servers.forgetActive()
    }
    maybeServer = nil
    maybeSignInPage = nil
    brand.select(nil)
    phase = .needsServer(address: "", failure: nil)
  }

  /// Asks the server what the sign-in screen offers (it may change at any time), each time the
  /// screen is shown; right after the server check its answer is used. Offline: SSO only.
  func refreshSignInPage() async {
    if isSignInPageFromCheck {
      isSignInPageFromCheck = false
      return
    }
    guard let server = maybeServer else { return }
    let brandName = brand.brand?.displayName ?? server.displayName
    let page = await SignInPageLoader(client: serverInfo).load(address: server.address, brandName: brandName)
    // Another server may have been chosen meanwhile.
    guard maybeServer?.address.origin == server.address.origin else { return }
    maybeSignInPage = page
  }

  func signIn() async {
    guard let server = maybeServer else {
      phase = .needsServer(address: "", failure: nil)
      return
    }
    await busy {
      do {
        let session = try await login.login(server: server)
        try vault.save(session)
        container.activeAccount = session.subject
        container.displayName = session.displayName
        await refreshAccountState()
      } catch OidcError.cancelled {
        phase = .signedOut(nil)
      } catch {
        phase = .signedOut(.signInFailed)
      }
    }
  }

  /// Signs in with a local account's e-mail address, password and optional one-time code. Returns
  /// why it failed for the form to show; `nil` after a sign-in (the phase moved on).
  func signInLocal(email: String, password: String, maybeCode: String?) async -> LocalSignInFailure? {
    guard let server = maybeServer, let origin = URL(string: server.address.origin) else {
      phase = .needsServer(address: "", failure: nil)
      return nil
    }
    var maybeFailure: LocalSignInFailure?
    await busy {
      let result = await localAuth.login(origin: origin, email: email, password: password, maybeCode: maybeCode)
      switch result {
      case .signedIn(let session):
        do {
          try vault.saveLocal(session)
        } catch {
          maybeFailure = .generic
          return
        }
        container.activeAccount = session.userId
        container.displayName = nil
        await refreshAccountState()
      case .failed(let failure):
        maybeFailure = failure
      }
    }
    return maybeFailure
  }

  /// Ends the session; the server stays, so signing in again is one tap ("Change server" leads to
  /// another one).
  func signOut() async {
    await busy {
      await endSession()
      phase = .signedOut(nil)
    }
  }

  // MARK: Enrollment

  func enroll(passphrase: String) async {
    guard case .needsEnrollment(let hasExisting, _) = phase else { return }
    await busy {
      do {
        let account = try await core().enroll(deviceName: deviceName, passphrase: passphrase)
        await adopt(account)
      } catch {
        phase = .needsEnrollment(hasExistingIdentity: hasExisting, failure: AppFailure(CoreFailure(error)))
      }
    }
  }

  /// New device: shows a code another device scans, waits for it, receives the keys (e2ee.md §5).
  func startLinking() async {
    await busy {
      do {
        let core = try self.core()
        let code = try await core.startDeviceLink()
        phase = .linking(LinkProgress(code: code))
        let shortCode = try await core.waitForLinkPeer()
        phase = .linking(LinkProgress(code: code, maybeShortCode: shortCode))
        let account = try await core.finishDeviceLink(deviceName: deviceName)
        await adopt(account)
      } catch {
        phase = .needsEnrollment(hasExistingIdentity: true, failure: AppFailure(CoreFailure(error)))
      }
    }
  }

  func cancelLinking() {
    phase = .needsEnrollment(hasExistingIdentity: true, failure: nil)
  }

  /// New device without the passphrase: asks the organization's admins (e2ee.md §6.3.1).
  func startRecovery() async {
    await busy {
      do {
        let start = try await core().startOrgRecovery()
        phase = .recovering(RecoveryProgress(verificationCode: start.verificationCode), failure: nil)
      } catch {
        phase = .needsEnrollment(hasExistingIdentity: true, failure: AppFailure(CoreFailure(error)))
      }
    }
  }

  func finishRecovery(newPassphrase: String) async {
    guard case .recovering(let progress, _) = phase else { return }
    await busy {
      do {
        let account = try await core().finishOrgRecovery(deviceName: deviceName, newPassphrase: newPassphrase)
        await adopt(account)
      } catch {
        phase = .recovering(progress, failure: AppFailure(CoreFailure(error)))
      }
    }
  }

  func cancelRecovery() {
    phase = .needsEnrollment(hasExistingIdentity: true, failure: nil)
  }

  /// Existing device: approves a new device by its code; returns the short code to compare.
  func approveLink(code: String) async throws -> String {
    try await core().approveDeviceLink(code: code.trimmingCharacters(in: .whitespacesAndNewlines))
  }

  func confirmLink() async throws {
    try await core().confirmDeviceLink()
  }

  // MARK: Reading

  func conversations() async -> [ConversationItem] {
    (try? await core().conversations()) ?? []
  }

  func messages(conversationId: String, limit: UInt32 = 200) async -> [MessageItem] {
    (try? await core().messages(conversationId: conversationId, limit: limit)) ?? []
  }

  func thread(conversationId: String, threadId: String) async -> [MessageItem] {
    (try? await core().thread(conversationId: conversationId, threadId: threadId)) ?? []
  }

  /// The newest message of each conversation, for the chat list. Local reads only.
  func chatPreviews(_ conversations: [ConversationItem], myId: String) async -> [String: ChatPreview] {
    var previews: [String: ChatPreview] = [:]
    for conversation in conversations {
      guard let latest = await messages(conversationId: conversation.id, limit: 1).last else { continue }
      previews[conversation.id] = chatPreview(latest, myId: myId, isOneToOne: conversation.kind == .direct)
    }
    return previews
  }

  func search(_ query: String) async -> [SearchHitItem] {
    guard !query.trimmingCharacters(in: .whitespaces).isEmpty else { return [] }
    return (try? await core().search(query: query)) ?? []
  }

  // MARK: Acting

  /// Sends a text message; `false` when the core refused it (the composer keeps the text then).
  @discardableResult
  func send(conversationId: String, threadId: String?, body: String) async -> Bool {
    do {
      try await core().sendMessage(conversationId: conversationId, maybeThreadId: threadId, body: body)
      return true
    } catch {
      return false
    }
  }

  func markRead(conversationId: String) async {
    try? await core().markRead(conversationId: conversationId)
    UNUserNotificationCenter.current().removeDeliveredNotifications(
      withIdentifiers: await deliveredIdentifiers(forConversation: conversationId))
  }

  func setFavorite(conversationId: String, isFavorite: Bool) async {
    try? await core().setFavorite(conversationId: conversationId, isFavorite: isFavorite)
  }

  func setPresence(_ presence: Presence) async {
    try? await core().setPresence(presence: presence)
  }

  func perform(_ action: ChatAction) async {
    try? await core().act(actionJson: action.json)
  }

  /// Like `perform`, but tells whether the core accepted the action.
  func performChecked(_ action: ChatAction) async -> Bool {
    do {
      try await core().act(actionJson: action.json)
      return true
    } catch {
      return false
    }
  }

  // MARK: Files

  /// Importer for picked files, with the server's upload limit if the core knows it.
  func attachmentImporter() async -> AttachmentImporter {
    AttachmentImporter(directory: container.uploadsDirectory, maybeLimitBytes: await maxUploadBytes())
  }

  func maxUploadBytes() async -> Int64? {
    await answer(.maxUploadBytes)?.maybeBytes
  }

  /// Files, link previews and other extras of the given messages, by message id.
  func extras(conversationId: String, messageIds: [String]) async -> [String: MessageExtras] {
    guard !messageIds.isEmpty,
      let extras = await answer(.messageExtras(conversationId: conversationId, messageIds: messageIds))?.extras
    else { return [:] }
    return Dictionary(extras.map { ($0.message, $0) }, uniquingKeysWith: { first, _ in first })
  }

  /// Every file of the conversation, newest first as the core lists them.
  func files(conversationId: String) async -> [FileInfo] {
    await answer(.conversationFiles(conversationId: conversationId))?.files ?? []
  }

  // MARK: Meetings

  /// The user's meetings that haven't ended, soonest first.
  func meetings() async -> [Meeting] {
    guard let json = try? await core().apiRequest(method: "GET", path: "/meetings", maybeBody: nil),
      let list = Wire.decode(MeetingList.self, from: json)
    else { return [] }
    return list.meetings.filter { $0.status == "scheduled" }.sorted { $0.startsAt < $1.startsAt }
  }

  func setRsvp(meetingId: String, answer: RsvpAnswer) async -> Bool {
    let body = jsonText(["status": answer.rawValue])
    do {
      _ = try await core().apiRequest(method: "PUT", path: "/meetings/\(meetingId)/rsvp", maybeBody: body)
      return true
    } catch {
      return false
    }
  }

  // MARK: Notification rules and prefetch

  func mentionSettings() async -> MentionSettings {
    (try? await core().mentionSettings())
      ?? MentionSettings(isFirstNameMention: true, doChannelMentionsNotify: true, keywords: [])
  }

  /// The core keeps them in the store, so the notification extension applies them too.
  func setMentionSettings(_ settings: MentionSettings) async {
    try? await core().setMentionSettings(settings: settings)
  }

  func segmentPrefetch() async -> SegmentPrefetchMode {
    (try? await core().segmentPrefetch()) ?? .unmeteredOnly
  }

  func setSegmentPrefetch(_ mode: SegmentPrefetchMode) async {
    try? await core().setSegmentPrefetch(mode: mode)
    if mode == .off {
      SegmentPrefetchTask.cancel()
    } else {
      SegmentPrefetchTask.schedule()
    }
  }

  /// Runs in a background processing task (power and network): downloads the search index
  /// segments this device doesn't have yet, if the setting allows it on this network. `true` when
  /// there was nothing to do or the download went through.
  func runSegmentPrefetch() async -> Bool {
    guard let core = try? core() else { return true }
    guard let mode = try? await core.segmentPrefetch(), mode != .off else { return true }
    let isMetered = await NetworkPath.isMetered()
    guard mode == .always || !isMetered else { return true }
    // The foreground app already holds the lock; otherwise take it like the extension does.
    if !isSyncing {
      let lock = processLock
      guard await Task.detached(operation: { lock.acquire(timeout: 5) }).value else { return false }
    }
    defer { if !isSyncing { processLock.release() } }
    return (try? await core.prefetchSegments(isNetworkMetered: isMetered, maxTotalBytes: Self.segmentPrefetchCapBytes))
      != nil
  }

  // MARK: Scene and sync

  /// Foreground: this process syncs (it holds the cross-process lock, docs/e2ee.md §7).
  func sceneBecameActive() async {
    guard case .ready = phase else { return }
    await startSyncing()
    await reconcileNotifications()
    // APNs hands out the token once per launch, but the app may stay suspended for days: retry a
    // registration that failed (server unreachable) and re-seal the token when it is due.
    if let token = maybePushToken { await registerPushIfNeeded(token: token) }
  }

  func sceneEnteredBackground() async {
    await stopSyncing()
    guard case .ready = phase else { return }
    if await segmentPrefetch() != .off { SegmentPrefetchTask.schedule() }
  }

  // MARK: Push

  // "v2": sealed for the push gateway (docs/push-gateway.md); a registration made by an older version
  // (the raw token) is replaced at the first launch.
  private static let pushTokenKey = "apnsToken.v2"
  /// Search index segments kept on the phone by the background prefetch (newest months first).
  private static let segmentPrefetchCapBytes: UInt64 = 512 * 1024 * 1024

  /// Asks for the permission and registers with APNs; the token arrives in `didRegisterForRemoteNotifications`.
  func enablePush() async {
    let center = UNUserNotificationCenter.current()
    NotificationCategories.register(in: center)
    let granted = (try? await center.requestAuthorization(options: [.alert, .sound, .badge])) ?? false
    pushStatus = granted ? .enabled : .denied
    if granted { UIApplication.shared.registerForRemoteNotifications() }
  }

  /// After sign-in: registers again when notifications are allowed. The first time, the chat list
  /// asks with a line on why (`NotificationPromptCard`) instead of the bare system prompt.
  private func resumePush() async {
    let settings = await UNUserNotificationCenter.current().notificationSettings()
    switch settings.authorizationStatus {
    case .notDetermined: pushStatus = .unknown
    case .denied: pushStatus = .denied
    default: await enablePush()
    }
  }

  /// The APNs device token: registers it with the server (a fresh push key for the extension).
  func didRegister(deviceToken: Data) async {
    let token = deviceToken.hexString
    maybePushToken = token
    guard case .ready = phase else { return }
    await registerPushIfNeeded(token: token)
  }

  /// Whether this device's push token is registered with the server (diagnostics).
  var isPushTokenRegistered: Bool {
    UserDefaults.standard.string(forKey: Self.pushTokenKey) != nil
  }

  /// A notification's tap or action (reply, mark as read, answer or decline a call).
  /// The conversation ID comes from the notification, which a forged push could fill: it is only
  /// acted on if the local store knows that conversation (the extension sets it from the decrypted
  /// payload, and drops it when it decrypted nothing).
  /// Reply, mark-as-read and answer also need the extension's tag over the ID
  /// (`NotificationAuthenticator`): the server can show an alert without the extension, naming a
  /// conversation the user is in.
  func handle(
    notificationAction identifier: String, conversationId: String, tag: String?, replyText: String?,
    maybeThreadId: String? = nil, notificationId: String? = nil
  ) async {
    guard let conversation = await conversations().first(where: { $0.id == conversationId }) else { return }
    let isAuthentic = NotificationAuthenticator().isValid(tag: tag, for: conversationId)
    switch identifier {
    case NotificationCategories.replyAction:
      if isAuthentic, let replyText, !replyText.isEmpty {
        await send(conversationId: conversationId, threadId: maybeThreadId, body: replyText)
      }
    case NotificationCategories.markReadAction:
      if isAuthentic { await markRead(conversationId: conversationId) }
    case NotificationCategories.answerCallAction:
      removeDelivered(notificationId)
      openConversationId = conversationId
      if isAuthentic { calls.startCall(conversationId: conversationId, displayName: conversation.name) }
    case NotificationCategories.declineCallAction:
      // There is no decline call in the core yet: the ring stops here, the caller rings on.
      removeDelivered(notificationId)
    default:
      openConversationId = conversationId
    }
  }

  /// Opens a sealed wake-up with the core in this process (a silent push, a VoIP push, or one the
  /// extension couldn't open because the app held the store). `nil` if it can't be read.
  func openPush(_ sealed: Data) async -> PushNotification? {
    guard let core = try? core() else { return nil }
    // In the foreground the app holds the lock already; otherwise take it like the extension does.
    let needsLock = !isSyncing
    if needsLock {
      let lock = processLock
      guard await Task.detached(operation: { lock.acquire(timeout: 5) }).value else { return nil }
    }
    defer { if needsLock && !isSyncing { processLock.release() } }
    return try? await core.notificationForPush(payload: sealed, utcOffsetMinutes: PushClock.utcOffsetMinutes())
  }

  /// A background (`content-available`) push: a `dismiss` removes what was read or ended elsewhere
  /// and sets the badge.
  func handleBackgroundPush(_ userInfo: [AnyHashable: Any]) async -> UIBackgroundFetchResult {
    guard let sealed = PushPayload.sealedBytes(from: userInfo) else { return .noData }
    guard let notification = await openPush(sealed) else { return .failed }
    switch notification.kind {
    case .dismiss:
      await DeliveredNotifications.apply(dismiss: notification)
      if notification.maybeCallId != nil {
        calls.endRinging(conversationId: notification.conversationId, maybeCallId: notification.maybeCallId)
      }
      return .newData
    case .message, .call, .test, .unknown:
      // Not sent as background pushes; only the badge is taken.
      await DeliveredNotifications.setBadge(notification.maybeBadge)
      return .noData
    }
  }

  /// The system asks the foreground app how to present a notification. One the extension built is
  /// shown unless its conversation is on screen; one it couldn't open (the app held the store) still
  /// has the sealed wake-up, which the app opens and shows itself.
  func presentation(for notification: UNNotification) async -> UNNotificationPresentationOptions {
    let content = notification.request.content
    guard let sealed = PushPayload.sealedBytes(from: content.userInfo) else {
      let conversationId = content.userInfo[NotificationCategories.conversationKey] as? String
      return conversationId != nil && conversationId == visibleConversationId ? [] : [.banner, .sound, .list]
    }
    guard let opened = await openPush(sealed) else { return [.banner, .sound, .list] }
    await present(opened, base: content)
    return []
  }

  // MARK: Delivered notifications and badge

  /// The store changed (this process's core, or the extension): screens re-read, and notifications of
  /// conversations that are read now go.
  private func storeDidChange() {
    changeCount += 1
    connection = maybeCore?.connectionState()
    guard maybeReconcile == nil else { return }
    maybeReconcile = Task { [weak self] in
      // Coalesces a burst of changes (a sync delivers many) into one pass.
      try? await Task.sleep(for: .milliseconds(500))
      await self?.reconcileNotifications()
      self?.maybeReconcile = nil
    }
  }

  /// Removes notifications of conversations the core reports fully read and sets the badge from the
  /// local unread state (while the app is open; in the background the wake-ups carry the badge).
  func reconcileNotifications() async {
    guard case .ready = phase, UIApplication.shared.applicationState != .background else { return }
    let all = await conversations()
    let read = Set(all.filter { $0.unreadCount == 0 && $0.mentionCount == 0 }.map(\.id))
    await DeliveredNotifications.removeRead(conversations: read)
    let summaries = all.map {
      UnreadSummary(
        isDirectMessage: $0.kind == .direct || $0.kind == .group, unreadCount: $0.unreadCount,
        mentionCount: $0.mentionCount)
    }
    await DeliveredNotifications.setBadge(UInt32(BadgeCount.local(summaries)))
  }

  /// Shows a push the app opened itself in the foreground, by the same rules as the extension (the
  /// app can drop a notification, so it behaves as with the filtering entitlement).
  private func present(_ notification: PushNotification, base: UNNotificationContent) async {
    let builder = NotificationContentBuilder(strings: .localized)
    let flags = PushFlags(notification)
    let maybeContent: UNNotificationContent?
    switch PushPresentation.of(flags, canFilter: true) {
    case .message, .quietMessage:
      guard notification.conversationId != visibleConversationId else { return }
      await DeliveredNotifications.removeEarlier(of: notification)
      maybeContent = builder.message(for: notification, presentation: .message, base: base).content
    case .test:
      maybeContent = builder.test(for: notification, base: base)
    case .callKit, .callAlert:
      // The open app rings through the gateway (`onIncomingCall`); nothing to add.
      maybeContent = nil
    case .drop, .quietGeneric:
      if notification.kind == .dismiss { await DeliveredNotifications.apply(dismiss: notification) }
      maybeContent = nil
    }
    await DeliveredNotifications.setBadge(flags.maybeBadge)
    guard let content = maybeContent else { return }
    let request = UNNotificationRequest(identifier: UUID().uuidString, content: content, trigger: nil)
    try? await UNUserNotificationCenter.current().add(request)
  }

  private func removeDelivered(_ maybeIdentifier: String?) {
    guard let identifier = maybeIdentifier else { return }
    UNUserNotificationCenter.current().removeDeliveredNotifications(withIdentifiers: [identifier])
  }

  // MARK: Calls

  /// Joins the conversation's call (CallKit asked: answered, or started by the user).
  func joinCall(conversationId: String) async throws -> CallSession {
    try await core().joinCall(conversationId: conversationId)
  }

  /// Where a message link points, if this device is in that conversation and has the message.
  func resolvePermalink(_ link: String) async -> PermalinkTarget? {
    await answer(.resolvePermalink(link: link))?.maybeTarget
  }

  /// The link to a message ("Copy link"), once the server has it.
  func permalink(conversationId: String, messageId: String) async -> String? {
    await answer(.permalink(conversationId: conversationId, messageId: messageId))?.maybeLink
  }

  /// Spaces, people and conversations for Browse.
  func browseSnapshot() async -> BrowseSnapshot? {
    guard let json = try? await core().snapshot() else { return nil }
    return Wire.decode(BrowseSnapshot.self, from: json)
  }

  func browseChannels(spaceId: String) async -> [BrowsableChannel] {
    await answer(.browseChannels(spaceId: spaceId))?.channels ?? []
  }

  /// Display names by user ID, for reply quotes.
  func displayNames() async -> [String: String] {
    guard let json = try? await core().snapshot(), let people = Wire.decode(PeopleSnapshot.self, from: json) else {
      return [:]
    }
    let everyone = (people.users ?? []) + [people.me]
    return Dictionary(everyone.map { ($0.id, $0.displayName) }, uniquingKeysWith: { first, _ in first })
  }

  /// Names of the people typing in a conversation right now.
  func typingNames(conversationId: String) async -> [String] {
    guard let ids = await answer(.typingUsers(conversationId: conversationId))?.users, !ids.isEmpty else { return [] }
    guard let json = try? await core().snapshot(), let people = Wire.decode(PeopleSnapshot.self, from: json) else {
      return []
    }
    let names = Dictionary((people.users ?? []).map { ($0.id, $0.displayName) }, uniquingKeysWith: { first, _ in first })
    return ids.map { names[$0] ?? "" }.filter { !$0.isEmpty }
  }

  /// Who can be mentioned (lower-case usernames) and the user's own username, for highlighting.
  func mentionNames() async -> (usernames: Set<String>, me: String) {
    guard let json = try? await core().snapshot(), let people = Wire.decode(PeopleSnapshot.self, from: json) else {
      return ([], "")
    }
    return (Set((people.users ?? []).map { $0.username.lowercased() }), people.me.username)
  }

  func conversationName(_ conversationId: String) async -> String {
    await conversations().first { $0.id == conversationId }?.name ?? "TeamCraft"
  }

  // MARK: Private

  /// The core of the signed-in account; opened on first use (a notification action may be the first).
  func core() throws -> TeamCraft {
    if let maybeCore { return maybeCore }
    guard let account = container.activeAccount, vault.hasSession else { throw CoreFactoryError.noAccount }
    let opened = try factory.open(
      account: account, createKey: true,
      listener: ClosureEventListener { [weak self] in
        Task { @MainActor in self?.storeDidChange() }
      })
    maybeCore = opened
    connection = opened.connectionState()
    return opened
  }

  /// The network changed: the open core checks its connection now. Opens no core.
  func networkChanged() {
    maybeCore?.networkChanged()
  }

  private func answer(_ query: ChatQuery) async -> ChatAnswer? {
    guard let json = try? await core().query(queryJson: query.json) else { return nil }
    return Wire.decode(ChatAnswer.self, from: json)
  }

  /// Picks the server at start: the stored one, a preconfigured one to confirm, or the entry.
  private func startWithServer() async {
    managedServer = ManagedServer.current()
    let stored = servers.activeServer
    // A stored entry the core no longer accepts is dropped; its server is entered again.
    if stored == nil { servers.forgetActive() }
    let decision = ServerStartDecision.decide(
      maybeStoredOrigin: stored?.address.origin, managed: managedServer, normalize: Self.normalizedOrigin)
    switch decision {
    case .useStored:
      maybeServer = stored
      brand.select(stored?.address)
      await refreshAccountState()
    case .forgetStoredAndCheck(let address):
      // The management locks another server: this account may not stay on the device.
      await endSession()
      servers.forgetActive()
      maybeServer = nil
      await checkServer(address)
    case .check(let address):
      await checkServer(address)
    case .enterServer:
      phase = .needsServer(address: "", failure: nil)
    }
  }

  /// The origin `address` normalises to, `nil` when unusable.
  nonisolated private static func normalizedOrigin(_ address: String) -> String? {
    try? normalizeServerAddress(input: address, allowInsecureHttp: AppEnvironment.allowsInsecureHttp).origin
  }

  /// Signs the active account out on this device: push registration, keys, session and local
  /// plaintext go; the server entry stays.
  private func endSession() async {
    await stopSyncing()
    // Before the keys go: deletes the push key here, ends the sealed device token at the gateway
    // and the registration on the server, so the server can't push to this phone any more.
    // Best effort and at most a few seconds; offline, the push key is still gone.
    if let core = try? core() {
      await Self.awaitAtMost(seconds: 5) { try? await core.unregisterPush() }
    }
    // A local session also ends on the server (best effort; offline, the token just expires).
    if let local = vault.loadLocal(),
      let origin = (maybeServer ?? servers.activeServer).flatMap({ URL(string: $0.address.origin) })
    {
      let client = localAuth
      let token = local.accessToken
      await Self.awaitAtMost(seconds: 5) { await client.logout(origin: origin, accessToken: token) }
    }
    if let account = container.activeAccount {
      factory.deleteDatabaseKey(account: account)
    }
    maybeCore = nil
    connection = nil
    vault.clear()
    NotificationAuthenticator().removeKey()
    container.wipeDownloads()
    container.pruneUploads(olderThan: 0)
    container.activeAccount = nil
    container.displayName = nil
    UserDefaults.standard.removeObject(forKey: Self.pushTokenKey)
  }

  /// Builds before server entry kept one session under a fixed name: it is deleted with its
  /// database key and data, and the user enters the server and signs in again.
  private func removeLegacyAccount() {
    guard vault.removeLegacySession() else { return }
    if let account = container.activeAccount {
      try? factory.keys.delete(for: account)
    }
    container.removeLegacyData()
    container.activeAccount = nil
    container.displayName = nil
    UserDefaults.standard.removeObject(forKey: Self.pushTokenKey)
  }

  private func refreshAccountState() async {
    guard maybeServer != nil, vault.hasSession, container.activeAccount != nil else {
      phase = .signedOut(nil)
      return
    }
    do {
      let state = try await core().accountState()
      if case .needsEnrollment(let hasExistingIdentity) = state {
        phase = .needsEnrollment(hasExistingIdentity: hasExistingIdentity, failure: nil)
      } else {
        await adopt(state)
      }
    } catch {
      let failure = CoreFailure(error)
      if failure == .signedOut {
        vault.clear()
        phase = .signedOut(.signedOut)
      } else {
        phase = .failed(AppFailure(failure))
      }
    }
  }

  private func adopt(_ state: AccountState) async {
    guard case .ready(let userId, let displayName, let fingerprint) = state else {
      phase = .needsEnrollment(hasExistingIdentity: true, failure: nil)
      return
    }
    container.displayName = displayName
    try? NotificationAuthenticator().ensureKey()
    phase = .ready(Account(userId: userId, displayName: displayName, identityFingerprint: fingerprint))
    if let core = try? core() {
      await calls.attach(core: core)
    }
    await startSyncing()
    await resumePush()
  }

  private func startSyncing() async {
    guard !isSyncing, let core = try? core() else { return }
    let lock = processLock
    // The extension may be handling a push; wait a little, then go on (SQLite serializes writes).
    let hasLock = await Task.detached { lock.acquire(timeout: 5) }.value
    _ = hasLock
    isSyncing = true
    try? await core.start()
  }

  private func stopSyncing() async {
    guard isSyncing else { return }
    isSyncing = false
    if let core = try? core() { try? await core.stop() }
    processLock.release()
  }

  /// Waits for `operation`, but no longer than `seconds`; a slower operation keeps running.
  private static func awaitAtMost(seconds: Double, _ operation: @escaping @Sendable () async -> Void) async {
    await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
      let gate = ResumeOnce(continuation)
      Task {
        await operation()
        gate.resume()
      }
      Task {
        try? await Task.sleep(for: .seconds(seconds))
        gate.resume()
      }
    }
  }

  private func registerPushIfNeeded(token: String) async {
    let defaults = UserDefaults.standard
    guard let core = try? core() else { return }
    guard defaults.string(forKey: Self.pushTokenKey) != token else {
      // Registered already: seals the token again when that is due (it is bound to one server and
      // expires), at most one request otherwise.
      _ = try? await core.refreshPush()
      return
    }
    do {
      try await core.registerPush(service: .apns, token: token)
      defaults.set(token, forKey: Self.pushTokenKey)
    } catch {
      // Not stored: the next foreground tries again.
    }
  }

  private func deliveredIdentifiers(forConversation conversationId: String) async -> [String] {
    let delivered = await UNUserNotificationCenter.current().deliveredNotifications()
    return delivered
      .filter { $0.request.content.threadIdentifier == conversationId }
      .map(\.request.identifier)
  }

  private func busy(_ work: () async -> Void) async {
    isBusy = true
    await work()
    isBusy = false
  }

  private var deviceName: String {
    UIDevice.current.name
  }
}

/// Resumes a continuation exactly once, whichever caller comes first.
private final class ResumeOnce: @unchecked Sendable {
  private let lock = NSLock()
  private var maybeContinuation: CheckedContinuation<Void, Never>?

  init(_ continuation: CheckedContinuation<Void, Never>) {
    maybeContinuation = continuation
  }

  func resume() {
    lock.lock()
    let continuation = maybeContinuation
    maybeContinuation = nil
    lock.unlock()
    continuation?.resume()
  }
}
