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
  /// The conversation on screen; its notifications are not shown as banners.
  var visibleConversationId: String?
  private(set) var pushStatus: PushStatus = .unknown

  let calls = CallManager()

  @ObservationIgnored private let vault = SessionVault()
  @ObservationIgnored private let container = SharedContainer()
  @ObservationIgnored private lazy var factory = CoreFactory(container: container, vault: vault)
  @ObservationIgnored private let login = WebLogin()
  @ObservationIgnored private lazy var processLock = ProcessLock(fileURL: container.lockFileURL)
  @ObservationIgnored private let storeChanges = StoreChangeNotifier()
  @ObservationIgnored private var maybeCore: TeamCraft?
  @ObservationIgnored private var isSyncing = false
  @ObservationIgnored private var maybePushToken: String?

  private init() {}

  enum PushStatus: Equatable {
    case unknown, enabled, denied
  }

  // MARK: Start and sign-in

  /// Opens the stored session, if any. Safe to call more than once.
  func bootstrap() async {
    guard case .starting = phase else { return }
    storeChanges.observe { [weak self] in self?.changeCount += 1 }
    await refreshAccountState()
  }

  /// After a failed start: tries to open the account again.
  func retry() async {
    phase = .starting
    await refreshAccountState()
  }

  func signIn() async {
    await busy {
      do {
        let session = try await login.login()
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

  func signOut() async {
    await busy {
      await stopSyncing()
      if let account = container.activeAccount {
        try? factory.keys.delete(for: account)
      }
      maybeCore = nil
      vault.clear()
      container.wipeDownloads()
      container.activeAccount = nil
      container.displayName = nil
      UserDefaults.standard.removeObject(forKey: Self.pushTokenKey)
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

  func search(_ query: String) async -> [SearchHitItem] {
    guard !query.trimmingCharacters(in: .whitespaces).isEmpty else { return [] }
    return (try? await core().search(query: query)) ?? []
  }

  // MARK: Acting

  func send(conversationId: String, threadId: String?, body: String) async {
    try? await core().sendMessage(conversationId: conversationId, maybeThreadId: threadId, body: body)
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

  // MARK: Scene and sync

  /// Foreground: this process syncs (it holds the cross-process lock, docs/e2ee.md §7).
  func sceneBecameActive() async {
    guard case .ready = phase else { return }
    await startSyncing()
  }

  func sceneEnteredBackground() async {
    await stopSyncing()
  }

  // MARK: Push

  private static let pushTokenKey = "apnsToken"

  /// Asks for the permission and registers with APNs; the token arrives in `didRegisterForRemoteNotifications`.
  func enablePush() async {
    let center = UNUserNotificationCenter.current()
    NotificationCategories.register(in: center)
    let granted = (try? await center.requestAuthorization(options: [.alert, .sound, .badge])) ?? false
    pushStatus = granted ? .enabled : .denied
    if granted { UIApplication.shared.registerForRemoteNotifications() }
  }

  /// The APNs device token: registers it with the server (a fresh push key for the extension).
  func didRegister(deviceToken: Data) async {
    let token = deviceToken.hexString
    maybePushToken = token
    guard case .ready = phase else { return }
    await registerPushIfNeeded(token: token)
  }

  /// A notification's tap or action (reply, mark as read).
  /// The conversation ID comes from the notification, which a forged push could fill: it is only
  /// acted on if the local store knows that conversation (the extension sets it from the decrypted
  /// payload, and drops it when it decrypted nothing).
  func handle(notificationAction identifier: String, conversationId: String, replyText: String?) async {
    guard await conversations().contains(where: { $0.id == conversationId }) else { return }
    switch identifier {
    case NotificationCategories.replyAction:
      if let replyText, !replyText.isEmpty {
        await send(conversationId: conversationId, threadId: nil, body: replyText)
      }
    case NotificationCategories.markReadAction:
      await markRead(conversationId: conversationId)
    default:
      openConversationId = conversationId
    }
  }

  // MARK: Calls

  /// Joins the conversation's call (CallKit asked: answered, or started by the user).
  func joinCall(conversationId: String) async throws -> CallSession {
    try await core().joinCall(conversationId: conversationId)
  }

  func conversationName(_ conversationId: String) async -> String {
    await conversations().first { $0.id == conversationId }?.name ?? "TeamCraft"
  }

  // MARK: Private

  /// The core of the signed-in account; opened on first use (a notification action may be the first).
  func core() throws -> TeamCraft {
    if let maybeCore { return maybeCore }
    guard let account = container.activeAccount, vault.load() != nil else { throw CoreFactoryError.noAccount }
    let opened = try factory.open(
      account: account, createKey: true,
      listener: ClosureEventListener { [weak self] in
        Task { @MainActor in self?.changeCount += 1 }
      })
    maybeCore = opened
    return opened
  }

  private func refreshAccountState() async {
    guard vault.load() != nil, container.activeAccount != nil else {
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
    phase = .ready(Account(userId: userId, displayName: displayName, identityFingerprint: fingerprint))
    if let core = try? core() {
      await calls.attach(core: core)
    }
    await startSyncing()
    await enablePush()
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

  private func registerPushIfNeeded(token: String) async {
    let defaults = UserDefaults.standard
    guard defaults.string(forKey: Self.pushTokenKey) != token, let core = try? core() else { return }
    do {
      try await core.registerPush(service: .apns, token: token)
      defaults.set(token, forKey: Self.pushTokenKey)
    } catch {
      // Not stored: the next launch tries again.
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
