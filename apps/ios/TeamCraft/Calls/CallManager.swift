import AVFAudio
import CallKit
import Foundation
import Observation
import PushKit

/// The call this device is in or rings for, as the in-call screen shows it.
struct ActiveCall: Identifiable, Equatable {
  enum Phase: Equatable, Hashable { case ringing, connecting, connected, reconnecting }

  let id: UUID
  /// Empty while a VoIP push is still being opened (the call was reported before it was readable).
  var conversationId: String
  /// The server's call, when a push named it (a later `dismiss` with it stops the ring).
  var maybeCallId: String?
  var displayName: String
  let isOutgoing: Bool
  var phase: Phase
  var isMuted = false
  /// End-to-end encrypted with every participant's device verified.
  var isSecured = false
  var participantCount: UInt32 = 0
  /// The people of the other devices in the call, each once (the call's verified roster).
  var participantUserIds: [String] = []
  /// Which user each other device in the call belongs to (to name a video tile).
  var userIdByDevice: [String: String] = [:]
  /// The users whose audio is forwarded right now, loudest first.
  var speakingUserIds: [String] = []
  /// The users all of whose devices are rejoining.
  var reconnectingUserIds: [String] = []
  /// The users who muted their microphone.
  var mutedUserIds: [String] = []
  /// The code participants compare to rule out a man in the middle (once secured).
  var maybePrivacyCode: String?
  /// Put on hold from the system's call UI: nothing is sent or played.
  var isOnHold = false
}

/// A move of the call into a meeting room that somebody else announced in the DM's encryption group
/// (docs/meetings.md, "A call becomes a meeting"); the call screen offers to follow it.
struct CallMoveOffer: Equatable {
  let fromConversationId: String
  let toConversationId: String
  let byName: String
  /// The added people this device knows by name.
  let addedNames: [String]
}

/// A move of the call from a DM into a meeting room, as the server or this device announced it.
private struct CallMove: Equatable {
  let from: String
  let to: String
}

/// A switch into another call waits for this device's end of the previous one.
private struct LeaveWaiter {
  let conversationId: String
  let continuation: CheckedContinuation<Void, Never>
}

/// A remote camera or screen track that is shown in the call.
struct RemoteVideoStream: Identifiable {
  let id: String
  let deviceId: String
  let isScreen: Bool
  let renderer: RemoteVideoRenderer
}

/// Calls through CallKit (docs/e2ee.md §8, PLAN.md §15): the system shows the incoming call screen
/// (lock screen, CarPlay, headset buttons), the Rust core runs the call. Incoming rings reach a
/// sleeping app as an encrypted push: the notification extension decrypts it and calls
/// `CXProvider.reportNewIncomingVoIPPushPayload`, which wakes the app into
/// `pushRegistry(_:didReceiveIncomingPushWith:)` and from there into CallKit (e2ee.md §7).
@Observable
final class CallManager: NSObject, @unchecked Sendable {
  /// How long a switch into a meeting room waits for the core to report the DM call's end before
  /// joining anyway (one call at a time keeps audio and its end sounds apart).
  private static let leaveTimeout: Duration = .seconds(5)

  /// Main-thread state; the in-call screen observes it.
  private(set) var activeCall: ActiveCall?
  /// Why the last call ended, shown briefly on the screen.
  private(set) var maybeEndNote: LocalizedStringResource?
  /// Tracks of other participants that arrived, in the order they were published.
  private(set) var remoteVideo: [RemoteVideoStream] = []
  /// Display names by user ID, for naming the people in the call.
  private(set) var names: [String: String] = [:]
  /// This device sends its camera.
  private(set) var isCameraOn = false
  /// The user refused camera access; the screen explains where to change it.
  private(set) var isCameraDenied = false
  /// A move into a meeting room somebody else announced; the screen offers to follow it.
  private(set) var maybeMoveOffer: CallMoveOffer?
  /// The call is being checked and switched into a meeting room.
  private(set) var isMoving = false
  /// Why following a move failed (the call stays where it is).
  private(set) var maybeMoveFailure: AppFailure?
  /// Local picture and sender of the camera.
  @ObservationIgnored let camera = CameraStreamer()

  @ObservationIgnored private let provider: CXProvider
  @ObservationIgnored private let controller = CXCallController()
  @ObservationIgnored private var maybeRegistry: PKPushRegistry?
  @ObservationIgnored private var maybeSession: CallSession?
  @ObservationIgnored private var maybePendingAction: CXAction?
  @ObservationIgnored private var wantsJoin = false
  @ObservationIgnored private var isAudioActive = false
  @ObservationIgnored private var isJoining = false
  @ObservationIgnored private var wantsVideoStart = false
  @ObservationIgnored private let renderers = RendererRegistry()
  /// A move the server (`call_move`) or this device announced; followed only once the encrypted
  /// announcement is in and `acceptCallMove` checked it.
  @ObservationIgnored private var maybeAnnouncedMove: CallMove?
  @ObservationIgnored private var maybeLeaveWaiter: LeaveWaiter?

  override init() {
    let configuration = CXProviderConfiguration()
    configuration.supportsVideo = true
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

  /// Starts (or joins) the conversation's call; with `withVideo` the camera goes on once connected.
  @MainActor
  func startCall(conversationId: String, displayName: String, withVideo: Bool = false) {
    guard activeCall == nil else { return }
    let uuid = UUID()
    let handle = CXHandle(type: .generic, value: conversationId)
    let action = CXStartCallAction(call: uuid, handle: handle)
    action.contactIdentifier = displayName
    action.isVideo = withVideo
    wantsVideoStart = withVideo
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

  /// Turns the camera on or off during a call (CallKit is told so the system UI shows a video call).
  @MainActor
  func setCameraEnabled(_ isEnabled: Bool) async {
    guard let session = maybeSession, activeCall != nil else { return }
    if isEnabled {
      guard await CameraStreamer.requestAccess() else {
        isCameraDenied = true
        return
      }
      camera.start(sending: session)
    } else {
      camera.stop()
    }
    isCameraOn = isEnabled
    reportHasVideo(isEnabled)
  }

  func flipCamera() {
    camera.flip()
  }

  func dismissCameraDenied() {
    isCameraDenied = false
  }

  func hangUp() {
    guard let call = activeCall else { return }
    controller.request(CXTransaction(action: CXEndCallAction(call: call.id)), completion: { _ in })
  }

  // MARK: Moving into a meeting room

  /// "Add people" in the DM or group DM call of `conversationId`: a meeting room with the people in
  /// the call and `added`, into which the call moves (docs/meetings.md, "A call becomes a
  /// meeting"). Throws when it failed; a retry with the same `requestKey` gets the same room.
  @MainActor
  func addPeople(conversationId: String, inCall: [String], added: [String], requestKey: String) async throws {
    guard activeCall?.conversationId == conversationId else { return }
    let room = try await AppModel.shared.addPeopleToCall(
      conversationId: conversationId, inCall: inCall, added: added, requestKey: requestKey)
    // The server's `call_move` reaches this device too, but the own announcement is in the store
    // already: follow it now (still through `acceptCallMove`).
    noteAnnouncedMove(CallMove(from: conversationId, to: room))
  }

  /// The user tapped "Join the meeting room" on the offered move.
  @MainActor
  func joinMovedCall() {
    guard let offer = maybeMoveOffer else { return }
    Task { await follow(CallMove(from: offer.fromConversationId, to: offer.toConversationId)) }
  }

  func dismissMoveFailure() {
    maybeMoveFailure = nil
  }

  /// The store changed: an announced move may have its encrypted announcement now.
  @MainActor
  func storeDidChange() {
    guard maybeAnnouncedMove != nil, maybeMoveOffer == nil, !isMoving else { return }
    Task { await checkAnnouncedMove() }
  }

  /// A move of the active call was announced. Nothing moves on this alone: the DM's encrypted
  /// announcement decides, asked now and again on later store changes.
  @MainActor
  private func noteAnnouncedMove(_ move: CallMove) {
    guard activeCall?.conversationId == move.from, maybeAnnouncedMove != move else { return }
    maybeAnnouncedMove = move
    maybeMoveOffer = nil
    Task { await checkAnnouncedMove() }
  }

  /// A move this device asked for is followed at once; any other (the user's own from another
  /// device included) is offered on the call screen.
  @MainActor
  private func checkAnnouncedMove() async {
    guard let move = maybeAnnouncedMove,
      let info = await AppModel.shared.callMoveOffer(from: move.from, to: move.to),
      maybeAnnouncedMove == move, activeCall?.conversationId == move.from
    else { return }
    if info.isRequestedHere {
      await follow(move)
      return
    }
    let names = await AppModel.shared.displayNames()
    guard maybeAnnouncedMove == move else { return }
    maybeMoveOffer = CallMoveOffer(
      fromConversationId: move.from, toConversationId: move.to,
      byName: names[info.byUserId] ?? String(localized: "call_move_someone"),
      addedNames: info.addedUserIds.map { names[$0] ?? String(localized: "call_move_someone") })
  }

  /// Checks the move in the core, then switches the call over. A refused move is dropped (the
  /// call stays); any other failure keeps it for another try.
  @MainActor
  private func follow(_ move: CallMove) async {
    guard !isMoving, activeCall?.conversationId == move.from else { return }
    isMoving = true
    defer { isMoving = false }
    do {
      try await AppModel.shared.acceptCallMove(from: move.from, to: move.to)
    } catch {
      let failure = AppFailure(CoreFailure(error))
      maybeMoveFailure = failure
      if failure == .callMoveRefused {
        maybeAnnouncedMove = nil
        maybeMoveOffer = nil
      }
      return
    }
    guard activeCall?.conversationId == move.from else { return }
    maybeAnnouncedMove = nil
    maybeMoveOffer = nil
    await switchCall(to: move.to)
  }

  /// Leaves the DM's call and joins the meeting room's with microphone, camera and hold as they
  /// were. The CallKit call stays; only the core's call underneath changes.
  @MainActor
  private func switchCall(to conversationId: String) async {
    guard var call = activeCall else { return }
    let previousConversationId = call.conversationId
    let wasMuted = call.isMuted
    let wasCameraOn = isCameraOn
    let name = await AppModel.shared.conversationName(conversationId)
    guard activeCall?.id == call.id else { return }
    call.conversationId = conversationId
    call.displayName = name
    call.phase = .connecting
    call.isSecured = false
    call.participantCount = 0
    call.participantUserIds = []
    call.userIdByDevice = [:]
    call.speakingUserIds = []
    call.reconnectingUserIds = []
    call.mutedUserIds = []
    call.maybePrivacyCode = nil
    activeCall = call
    camera.stop()
    isCameraOn = false
    renderers.removeAll()
    remoteVideo = []
    if let previous = maybeSession {
      maybeSession = nil
      await leave(previous, conversationId: previousConversationId)
    }
    do {
      // Muted from the first frame: no audio slips out before the mute is applied.
      let session = try await AppModel.shared.joinCall(conversationId: conversationId, isMuted: wasMuted)
      guard activeCall?.id == call.id else {
        // Hung up while joining.
        session.leave()
        return
      }
      maybeSession = session
      configure(session)
      if call.isOnHold { session.setHeld(isHeld: true) }
      if var current = activeCall {
        current.phase = .connected
        activeCall = current
      }
      if wasCameraOn {
        camera.start(sending: session)
        isCameraOn = true
      }
      let update = CXCallUpdate()
      update.remoteHandle = CXHandle(type: .generic, value: conversationId)
      update.localizedCallerName = name
      provider.reportCall(with: call.id, updated: update)
    } catch {
      failCall(call, reason: .failed)
    }
  }

  /// Leaves `session` and waits until the core reported its end (or `leaveTimeout` passed), so the
  /// next call starts after the previous one is gone.
  @MainActor
  private func leave(_ session: CallSession, conversationId: String) async {
    await withCheckedContinuation { continuation in
      maybeLeaveWaiter = LeaveWaiter(conversationId: conversationId, continuation: continuation)
      session.leave()
      Task { @MainActor in
        try? await Task.sleep(for: Self.leaveTimeout)
        self.resumeLeaveWaiter(for: conversationId)
      }
    }
  }

  /// On the main thread, like the rest of the call state.
  private func resumeLeaveWaiter(for conversationId: String) {
    guard let waiter = maybeLeaveWaiter, waiter.conversationId == conversationId else { return }
    maybeLeaveWaiter = nil
    waiter.continuation.resume()
  }

  // MARK: Incoming

  /// Shows the system's incoming call screen. `completion` runs once CallKit has the call (PushKit
  /// requires that before it returns).
  func reportIncoming(
    conversationId: String, displayName: String, maybeCallId: String? = nil, completion: (() -> Void)? = nil
  ) {
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
      id: uuid, conversationId: conversationId, maybeCallId: maybeCallId, displayName: displayName,
      isOutgoing: false, phase: .ringing)
    provider.reportNewIncomingCall(with: uuid, update: update) { [weak self] error in
      if error != nil { self?.clear() }
      completion?()
    }
  }

  /// A `dismiss` wake-up for a call: the ring ended (answered elsewhere, declined, timed out), so
  /// this phone stops ringing. A call that was already answered here goes on.
  @MainActor
  func endRinging(conversationId: String, maybeCallId: String?) {
    guard let call = activeCall, call.phase == .ringing, !call.isOutgoing else { return }
    var isSameCall = call.conversationId == conversationId
    if let callId = maybeCallId, let ringing = call.maybeCallId { isSameCall = callId == ringing }
    guard isSameCall else { return }
    provider.reportCall(with: call.id, endedAt: Date(), reason: .answeredElsewhere)
    clear()
  }

  // MARK: Joining

  /// Joins in the core once CallKit activated the audio session and an answer or start is pending.
  private func joinIfReady() {
    guard wantsJoin, isAudioActive, !isJoining, maybeSession == nil, let call = activeCall,
      !call.conversationId.isEmpty
    else { return }
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
        configure(session)
        if var current = activeCall {
          current.phase = .connected
          activeCall = current
        }
        if call.isOutgoing { provider.reportOutgoingCall(with: call.id, connectedAt: Date()) }
        maybePendingAction?.fulfill()
        maybePendingAction = nil
        if wantsVideoStart {
          wantsVideoStart = false
          await setCameraEnabled(true)
        }
      } catch {
        failCall(call, reason: .failed)
      }
    }
  }

  /// What every joined session of this manager gets: the status sounds and the encoded video path.
  private func configure(_ session: CallSession) {
    session.setStatusSounds(sounds: CallStatusSoundsSetting.current.sounds)
    session.setEncodedVideoListener(listener: self)
  }

  private func requestMicrophone() async -> Bool {
    await withCheckedContinuation { continuation in
      AVAudioApplication.requestRecordPermission { continuation.resume(returning: $0) }
    }
  }

  /// `playAndRecord` for calls; a call that starts with video uses the video chat mode (speaker by default).
  private func configureAudioSession(video: Bool = false) {
    let session = AVAudioSession.sharedInstance()
    let options: AVAudioSession.CategoryOptions = video ? [.allowBluetooth, .defaultToSpeaker] : [.allowBluetooth]
    try? session.setCategory(.playAndRecord, mode: video ? .videoChat : .voiceChat, options: options)
  }

  @MainActor
  private func reportHasVideo(_ hasVideo: Bool) {
    guard let call = activeCall else { return }
    let update = CXCallUpdate()
    update.hasVideo = hasVideo
    provider.reportCall(with: call.id, updated: update)
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
      self.camera.stop()
      self.renderers.removeAll()
      self.remoteVideo = []
      self.isCameraOn = false
      self.wantsVideoStart = false
      self.maybeSession = nil
      self.wantsJoin = false
      self.isAudioActive = false
      self.maybeAnnouncedMove = nil
      self.maybeMoveOffer = nil
      self.maybeMoveFailure = nil
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
    configureAudioSession(video: action.isVideo)
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

  /// Hold silences the microphone, stops playing the others and pauses the camera; the call stays
  /// joined, so resuming is instant.
  func provider(_ provider: CXProvider, perform action: CXSetHeldCallAction) {
    maybeSession?.setHeld(isHeld: action.isOnHold)
    camera.setPaused(action.isOnHold)
    if var call = activeCall {
      call.isOnHold = action.isOnHold
      activeCall = call
    }
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

  /// Two sources: the extension's forward (already decrypted: conversation and caller name), or a
  /// VoIP push from the server with the sealed wake-up under `w`. Every VoIP push must end in a
  /// reported call before this returns, or iOS stops waking the app: the sealed one is reported
  /// first with a placeholder name and filled in (or ended) once the core opened it.
  func pushRegistry(
    _ registry: PKPushRegistry, didReceiveIncomingPushWith payload: PKPushPayload, for type: PKPushType,
    completion: @escaping () -> Void
  ) {
    let info = payload.dictionaryPayload
    if let sealed = PushPayload.sealedBytes(from: info) {
      reportSealedIncoming(sealed, completion: completion)
      return
    }
    let conversationId = (info[CallForward.conversationKey] as? String) ?? ""
    let name = (info[CallForward.callerNameKey] as? String) ?? String(localized: "app_name")
    let maybeCallId = info[CallForward.callIdKey] as? String
    if activeCall != nil {
      reportBusyRing(completion: completion)
      return
    }
    reportIncoming(conversationId: conversationId, displayName: name, maybeCallId: maybeCallId, completion: completion)
  }

  /// CallKit holds one call: a VoIP push during a call is reported and ended at once, only to
  /// satisfy PushKit.
  private func reportBusyRing(completion: @escaping () -> Void) {
    let uuid = UUID()
    let update = CXCallUpdate()
    update.localizedCallerName = String(localized: "call_incoming_placeholder")
    provider.reportNewIncomingCall(with: uuid, update: update) { [weak self] error in
      completion()
      if error == nil { self?.provider.reportCall(with: uuid, endedAt: Date(), reason: .unanswered) }
    }
  }

  private func reportSealedIncoming(_ sealed: Data, completion: @escaping () -> Void) {
    if activeCall != nil {
      reportBusyRing(completion: completion)
      return
    }
    let uuid = UUID()
    let update = CXCallUpdate()
    update.localizedCallerName = String(localized: "call_incoming_placeholder")
    update.hasVideo = false
    update.supportsHolding = true
    update.supportsGrouping = false
    update.supportsUngrouping = false
    update.supportsDTMF = false
    configureAudioSession()
    activeCall = ActiveCall(
      id: uuid, conversationId: "", displayName: String(localized: "call_incoming_placeholder"),
      isOutgoing: false, phase: .ringing)
    provider.reportNewIncomingCall(with: uuid, update: update) { [weak self] error in
      completion()
      guard let self else { return }
      if error != nil {
        self.clear()
        return
      }
      Task { @MainActor in await self.resolveSealedCall(uuid: uuid, sealed: sealed) }
    }
  }

  /// Fills in the reported call from the opened wake-up, or ends it if it isn't a ring to show
  /// (unreadable, not a call, already shown, or silenced by mute or DND on this device).
  @MainActor
  private func resolveSealedCall(uuid: UUID, sealed: Data) async {
    let maybeNotification = await AppModel.shared.openPush(sealed)
    guard var call = activeCall, call.id == uuid else { return }
    guard let notification = maybeNotification, notification.kind == .call, !notification.isDuplicate,
      notification.maybeSuppression == nil
    else {
      provider.reportCall(with: uuid, endedAt: Date(), reason: maybeNotification == nil ? .failed : .remoteEnded)
      clear()
      return
    }
    var name = notification.maybeSenderName ?? notification.maybeConversationName ?? ""
    if name.isEmpty { name = await AppModel.shared.conversationName(notification.conversationId) }
    call.conversationId = notification.conversationId
    call.maybeCallId = notification.maybeCallId
    call.displayName = name
    activeCall = call
    let update = CXCallUpdate()
    update.remoteHandle = CXHandle(type: .generic, value: notification.conversationId)
    update.localizedCallerName = name
    provider.reportCall(with: uuid, updated: update)
    // Answered while it was being opened: join now.
    joinIfReady()
  }
}

// MARK: - The core's call events

extension CallManager: CallListener {
  func onCallState(state: CallStateInfo) {
    DispatchQueue.main.async {
      if state.phase == .ended { self.resumeLeaveWaiter(for: state.conversationId) }
      guard var call = self.activeCall, call.conversationId == state.conversationId else { return }
      call.isMuted = state.isMuted
      call.isSecured = state.isSecured
      call.participantCount = state.participantCount
      let hasNewPeople = call.participantUserIds != state.participantUserIds
      call.participantUserIds = state.participantUserIds
      call.userIdByDevice = Dictionary(
        state.peers.map { ($0.deviceId, $0.userId) }, uniquingKeysWith: { first, _ in first })
      call.speakingUserIds = state.speakingUserIds
      call.reconnectingUserIds = state.reconnectingUserIds
      call.mutedUserIds = state.mutedUserIds
      if hasNewPeople { Task { @MainActor in self.names = await AppModel.shared.displayNames() } }
      call.maybePrivacyCode = state.maybePrivacyCode
      switch state.phase {
      case .connected: call.phase = .connected
      // The connection broke; the core rejoins (the others keep this device's place meanwhile).
      case .reconnecting: call.phase = .reconnecting
      case .connecting, .ended: break
      }
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

  /// The server says the call moved into a meeting room; only the encrypted announcement makes it
  /// an offer (or, for the user's own move, followed at once).
  func onCallMoved(fromConversationId: String, toConversationId: String) {
    Task { @MainActor in
      self.noteAnnouncedMove(CallMove(from: fromConversationId, to: toConversationId))
    }
  }

  private static func reason(for end: CallEndReason?) -> CXCallEndedReason {
    switch end {
    case .failed, .serverLost: return .failed
    case .removedByHost, .endedByHost: return .remoteEnded
    default: return .remoteEnded
    }
  }

  private static func note(for end: CallEndReason?) -> LocalizedStringResource? {
    switch end {
    case .failed: return "call_failed"
    case .serverLost: return "call_server_lost"
    case .removedByHost: return "call_removed_by_host"
    case .endedByHost: return "call_ended_by_host"
    default: return nil
    }
  }
}

// MARK: - Video from the core

/// Remote tracks and keyframe requests of the core's call session (called on the call thread).
extension CallManager: EncodedVideoListener {
  func onRemoteVideoTrack(track: RemoteVideoTrack) {
    let renderer = renderers.renderer(for: track.streamId, creating: true)
    DispatchQueue.main.async {
      guard let renderer, self.maybeSession != nil else { return }
      if !self.remoteVideo.contains(where: { $0.id == track.streamId }) {
        self.remoteVideo.append(
          RemoteVideoStream(
            id: track.streamId, deviceId: track.deviceId, isScreen: track.isScreen,
            renderer: renderer))
      }
      self.maybeSession?.watchEncodedVideo(streamId: track.streamId)
    }
  }

  func onRemoteVideoEnded(streamId: String) {
    renderers.remove(streamId)
    DispatchQueue.main.async { self.remoteVideo.removeAll { $0.id == streamId } }
  }

  func onRemoteVideoFrame(frame: RemoteVideoFrame) {
    renderers.renderer(for: frame.streamId, creating: false)?.push(data: frame.data, timestamp: frame.timestamp)
  }

  func onKeyframeRequested() {
    camera.requestKeyframe()
  }
}

/// The renderers of the remote tracks, shared between the call thread (frames) and the main thread
/// (screens).
final class RendererRegistry: @unchecked Sendable {
  private let lock = NSLock()
  private var renderers: [String: RemoteVideoRenderer] = [:]

  func renderer(for streamId: String, creating: Bool) -> RemoteVideoRenderer? {
    lock.lock()
    defer { lock.unlock() }
    if let existing = renderers[streamId] { return existing }
    guard creating else { return nil }
    let created = RemoteVideoRenderer()
    renderers[streamId] = created
    return created
  }

  func remove(_ streamId: String) {
    lock.lock()
    let removed = renderers.removeValue(forKey: streamId)
    lock.unlock()
    removed?.reset()
  }

  func removeAll() {
    lock.lock()
    let all = renderers
    renderers = [:]
    lock.unlock()
    for renderer in all.values { renderer.reset() }
  }
}
