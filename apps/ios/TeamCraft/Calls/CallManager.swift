import AVFAudio
import CallKit
import Foundation
import Observation
import PushKit

/// The call this device is in or rings for, as the in-call screen shows it.
struct ActiveCall: Identifiable, Equatable {
  enum Phase: Equatable { case ringing, connecting, connected }

  let id: UUID
  let conversationId: String
  var displayName: String
  let isOutgoing: Bool
  var phase: Phase
  var isMuted = false
  /// End-to-end encrypted with every participant's device verified.
  var isSecured = false
  var participantCount: UInt32 = 0
  /// The code participants compare to rule out a man in the middle (once secured).
  var maybePrivacyCode: String?
}

/// Calls through CallKit (docs/e2ee.md §8, PLAN.md §15): the system shows the incoming call screen
/// (lock screen, CarPlay, headset buttons), the Rust core runs the call. Incoming rings reach a
/// sleeping app as an encrypted push: the notification extension decrypts it and calls
/// `CXProvider.reportNewIncomingVoIPPushPayload`, which wakes the app into
/// `pushRegistry(_:didReceiveIncomingPushWith:)` and from there into CallKit (e2ee.md §7).
@Observable
final class CallManager: NSObject, @unchecked Sendable {
  /// Keys of the payload the extension forwards.
  static let conversationKey = "conversation_id"
  static let callerNameKey = "caller_name"

  /// Main-thread state; the in-call screen observes it.
  private(set) var activeCall: ActiveCall?
  /// Why the last call ended, shown briefly on the screen.
  private(set) var maybeEndNote: LocalizedStringResource?

  @ObservationIgnored private let provider: CXProvider
  @ObservationIgnored private let controller = CXCallController()
  @ObservationIgnored private var maybeRegistry: PKPushRegistry?
  @ObservationIgnored private var maybeSession: CallSession?
  @ObservationIgnored private var maybePendingAction: CXAction?
  @ObservationIgnored private var wantsJoin = false
  @ObservationIgnored private var isAudioActive = false
  @ObservationIgnored private var isJoining = false

  override init() {
    let configuration = CXProviderConfiguration()
    configuration.supportsVideo = false
    configuration.maximumCallGroups = 1
    configuration.maximumCallsPerCallGroup = 1
    configuration.supportedHandleTypes = [.generic]
    configuration.includesCallsInRecents = false
    provider = CXProvider(configuration: configuration)
    super.init()
    provider.setDelegate(self, queue: .main)
  }

  /// Registers for the forwarded wake-ups; call at launch, before the app finished starting, so a
  /// push that launches the app is delivered.
  func start() {
    guard maybeRegistry == nil else { return }
    let registry = PKPushRegistry(queue: .main)
    registry.delegate = self
    registry.desiredPushTypes = [.voIP]
    maybeRegistry = registry
  }

  /// Once the core is open: rings of the gateway and the call state come to this manager.
  func attach(core: TeamCraft) async {
    try? await core.setCallListener(listener: self)
  }

  // MARK: Commands from the app

  /// Starts (or joins) the conversation's call.
  @MainActor
  func startCall(conversationId: String, displayName: String) {
    guard activeCall == nil else { return }
    let uuid = UUID()
    let handle = CXHandle(type: .generic, value: conversationId)
    let action = CXStartCallAction(call: uuid, handle: handle)
    action.contactIdentifier = displayName
    activeCall = ActiveCall(
      id: uuid, conversationId: conversationId, displayName: displayName, isOutgoing: true,
      phase: .connecting)
    controller.request(CXTransaction(action: action)) { [weak self] error in
      if error != nil { self?.clear() }
    }
  }

  func setMuted(_ isMuted: Bool) {
    guard let call = activeCall else { return }
    controller.request(CXTransaction(action: CXSetMutedCallAction(call: call.id, muted: isMuted)), completion: { _ in })
  }

  func hangUp() {
    guard let call = activeCall else { return }
    controller.request(CXTransaction(action: CXEndCallAction(call: call.id)), completion: { _ in })
  }

  // MARK: Incoming

  /// Shows the system's incoming call screen. `completion` runs once CallKit has the call (PushKit
  /// requires that before it returns).
  func reportIncoming(conversationId: String, displayName: String, completion: (() -> Void)? = nil) {
    if activeCall != nil {
      // Already in a call (or ringing for one): CallKit allows one; a second ring isn't shown.
      completion?()
      return
    }
    let uuid = UUID()
    let update = CXCallUpdate()
    update.remoteHandle = CXHandle(type: .generic, value: conversationId)
    update.localizedCallerName = displayName
    update.hasVideo = false
    update.supportsHolding = true
    update.supportsGrouping = false
    update.supportsUngrouping = false
    update.supportsDTMF = false
    configureAudioSession()
    activeCall = ActiveCall(
      id: uuid, conversationId: conversationId, displayName: displayName, isOutgoing: false,
      phase: .ringing)
    provider.reportNewIncomingCall(with: uuid, update: update) { [weak self] error in
      if error != nil { self?.clear() }
      completion?()
    }
  }

  // MARK: Joining

  /// Joins in the core once CallKit activated the audio session and an answer or start is pending.
  private func joinIfReady() {
    guard wantsJoin, isAudioActive, !isJoining, maybeSession == nil, let call = activeCall else { return }
    isJoining = true
    Task { @MainActor in
      defer { isJoining = false }
      guard await requestMicrophone() else {
        failCall(call, reason: .failed)
        return
      }
      do {
        let session = try await AppModel.shared.joinCall(conversationId: call.conversationId)
        maybeSession = session
        if var current = activeCall {
          current.phase = .connected
          activeCall = current
        }
        if call.isOutgoing { provider.reportOutgoingCall(with: call.id, connectedAt: Date()) }
        maybePendingAction?.fulfill()
        maybePendingAction = nil
      } catch {
        failCall(call, reason: .failed)
      }
    }
  }

  private func requestMicrophone() async -> Bool {
    await withCheckedContinuation { continuation in
      AVAudioApplication.requestRecordPermission { continuation.resume(returning: $0) }
    }
  }

  private func configureAudioSession() {
    let session = AVAudioSession.sharedInstance()
    try? session.setCategory(.playAndRecord, mode: .voiceChat, options: [.allowBluetooth])
  }

  @MainActor
  private func failCall(_ call: ActiveCall, reason: CXCallEndedReason) {
    maybePendingAction?.fail()
    maybePendingAction = nil
    provider.reportCall(with: call.id, endedAt: Date(), reason: reason)
    maybeEndNote = "call_failed"
    clear()
  }

  private func clear() {
    DispatchQueue.main.async {
      self.maybeSession = nil
      self.wantsJoin = false
      self.isAudioActive = false
      self.activeCall = nil
    }
  }
}

// MARK: - CallKit

extension CallManager: CXProviderDelegate {
  func providerDidReset(_ provider: CXProvider) {
    maybeSession?.leave()
    clear()
  }

  func provider(_ provider: CXProvider, perform action: CXStartCallAction) {
    configureAudioSession()
    provider.reportOutgoingCall(with: action.callUUID, startedConnectingAt: nil)
    maybePendingAction = action
    wantsJoin = true
    joinIfReady()
  }

  func provider(_ provider: CXProvider, perform action: CXAnswerCallAction) {
    configureAudioSession()
    if var call = activeCall {
      call.phase = .connecting
      activeCall = call
    }
    maybePendingAction = action
    wantsJoin = true
    joinIfReady()
  }

  func provider(_ provider: CXProvider, perform action: CXEndCallAction) {
    maybeSession?.leave()
    maybePendingAction?.fail()
    maybePendingAction = nil
    clear()
    action.fulfill()
  }

  func provider(_ provider: CXProvider, perform action: CXSetMutedCallAction) {
    maybeSession?.setMuted(isMuted: action.isMuted)
    if var call = activeCall {
      call.isMuted = action.isMuted
      activeCall = call
    }
    action.fulfill()
  }

  /// The core has no hold; holding mutes, resuming unmutes.
  func provider(_ provider: CXProvider, perform action: CXSetHeldCallAction) {
    maybeSession?.setMuted(isMuted: action.isOnHold)
    action.fulfill()
  }

  func provider(_ provider: CXProvider, didActivate audioSession: AVAudioSession) {
    isAudioActive = true
    joinIfReady()
  }

  func provider(_ provider: CXProvider, didDeactivate audioSession: AVAudioSession) {
    isAudioActive = false
  }
}

// MARK: - PushKit (the extension's forwarded wake-up)

extension CallManager: PKPushRegistryDelegate {
  func pushRegistry(_ registry: PKPushRegistry, didUpdate pushCredentials: PKPushCredentials, for type: PKPushType) {
    // The server pushes through APNs only; this registry exists to receive the extension's forward.
  }

  func pushRegistry(
    _ registry: PKPushRegistry, didReceiveIncomingPushWith payload: PKPushPayload, for type: PKPushType,
    completion: @escaping () -> Void
  ) {
    let info = payload.dictionaryPayload
    let conversationId = (info[Self.conversationKey] as? String) ?? ""
    let name = (info[Self.callerNameKey] as? String) ?? "TeamCraft"
    // Every VoIP push must end in a reported call, or the system stops waking the app.
    reportIncoming(conversationId: conversationId, displayName: name, completion: completion)
  }
}

// MARK: - The core's call events

extension CallManager: CallListener {
  func onCallState(state: CallStateInfo) {
    DispatchQueue.main.async {
      guard var call = self.activeCall, call.conversationId == state.conversationId else { return }
      call.isMuted = state.isMuted
      call.isSecured = state.isSecured
      call.participantCount = state.participantCount
      call.maybePrivacyCode = state.maybePrivacyCode
      if state.phase == .connected { call.phase = .connected }
      self.activeCall = call
      if state.phase == .ended {
        self.maybeEndNote = Self.note(for: state.maybeEndReason)
        self.provider.reportCall(with: call.id, endedAt: Date(), reason: Self.reason(for: state.maybeEndReason))
        self.maybeSession = nil
        self.clear()
      }
    }
  }

  func onIncomingCall(call: IncomingCallInfo) {
    Task { @MainActor in
      let name = await AppModel.shared.conversationName(call.conversationId)
      self.reportIncoming(conversationId: call.conversationId, displayName: name)
    }
  }

  func onRingEnded(conversationId: String) {
    DispatchQueue.main.async {
      guard let call = self.activeCall, call.conversationId == conversationId, call.phase == .ringing else { return }
      // Answered on another device, declined or over: stop ringing here.
      self.provider.reportCall(with: call.id, endedAt: Date(), reason: .answeredElsewhere)
      self.clear()
    }
  }

  private static func reason(for end: CallEndReason?) -> CXCallEndedReason {
    switch end {
    case .failed: return .failed
    case .removedByHost, .endedByHost: return .remoteEnded
    default: return .remoteEnded
    }
  }

  private static func note(for end: CallEndReason?) -> LocalizedStringResource? {
    switch end {
    case .failed: return "call_failed"
    case .removedByHost: return "call_removed_by_host"
    case .endedByHost: return "call_ended_by_host"
    default: return nil
    }
  }
}
