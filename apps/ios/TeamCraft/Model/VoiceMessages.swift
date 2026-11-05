import AVFoundation
import Foundation
import Observation
import UIKit

/// Voice messages (#204). Recording goes through the core (`VoiceRecording` of teamcraft-ffi: the
/// microphone, Opus in Ogg, written to the private takes folder); the app asks for the permission,
/// owns the audio session and stops a recording on a call, the background or the lock. A take is
/// never sent by itself: the user sends or discards it. A voice message is an ordinary encrypted
/// attachment whose length and waveform travel inside the message.
///
/// Playback: AVFoundation reads neither Ogg nor WebM, so the core decodes such a message into a WAV
/// file in the private playback folder, which lives only while the player holds it.

/// The private folders of voice messages, in the caches (not in backups), emptied at every start: a
/// take left by a crash or a killed app is never sent.
enum VoiceFolders {
  static var takes: URL { prepared("voice-takes") }
  static var playback: URL { prepared("voice-playback") }

  static func clearAtStart() {
    for folder in [takes, playback] {
      let entries = (try? FileManager.default.contentsOfDirectory(at: folder, includingPropertiesForKeys: nil)) ?? []
      for entry in entries { try? FileManager.default.removeItem(at: entry) }
    }
  }

  private static func prepared(_ name: String) -> URL {
    var url = FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask)[0]
      .appendingPathComponent(name, isDirectory: true)
    try? FileManager.default.createDirectory(
      at: url, withIntermediateDirectories: true,
      attributes: [.protectionKey: FileProtectionType.completeUntilFirstUserAuthentication])
    var values = URLResourceValues()
    values.isExcludedFromBackup = true
    try? url.setResourceValues(values)
    return url
  }
}

/// The speeds a voice message plays at; the choice is remembered on the device.
enum VoiceSpeed: String, CaseIterable, Sendable {
  case normal = "1"
  case oneAndHalf = "1.5"
  case double = "2"

  static let key = "voice_speed"

  var rate: Float {
    switch self {
    case .normal: return 1
    case .oneAndHalf: return 1.5
    case .double: return 2
    }
  }

  /// What a press on the speed button goes to.
  var next: VoiceSpeed {
    switch self {
    case .normal: return .oneAndHalf
    case .oneAndHalf: return .double
    case .double: return .normal
    }
  }

  /// The button's text: a number, the same in every language.
  var label: String { "\(rawValue)×" }

  static func stored(_ raw: String) -> VoiceSpeed { VoiceSpeed(rawValue: raw) ?? .normal }
}

/// What a finger held on the record button does when it lets go: send, cancel (slid to the left)
/// or keep recording hands-free (slid up).
enum VoiceHoldGesture {
  /// Held this long, a press records; shorter is a tap (hands-free recording).
  static let holdDelay = Duration.milliseconds(250)
  static let cancelDistance: CGFloat = 100
  static let lockDistance: CGFloat = 70

  enum Outcome: Equatable {
    case send
    case cancel
    case lock
  }

  static func outcome(translation: CGSize) -> Outcome {
    let up = -translation.height
    let left = -translation.width
    if up >= lockDistance && up > left { return .lock }
    if left >= cancelDistance { return .cancel }
    return .send
  }
}

/// A finished take, waiting for the user to send or discard it.
struct VoiceTake: Equatable {
  let path: String
  let durationMs: Int64
  let waveform: [UInt8]
  let maybeStop: VoiceStop?
}

/// The app's one voice recording; a take belongs to the composer that recorded it.
@MainActor
@Observable
final class VoiceRecorder {
  static let shared = VoiceRecorder()
  /// The explanation before the system's first microphone question was shown.
  static let explainedKey = "voice_microphone_explained"
  /// Recent levels the recording bar draws.
  static let shownLevels = 32

  enum Refusal: Equatable {
    /// The microphone permission is refused: Settings has the switch.
    case permission
    /// The microphone didn't start (busy, gone).
    case failed
  }

  enum Phase: Equatable {
    case idle
    /// Before the system asks for the microphone the first time: what it is for.
    case explaining
    /// The microphone opens.
    case starting(isLocked: Bool)
    case recording(isLocked: Bool)
    case finishing
    case review(VoiceTake)
    case refused(Refusal)
  }

  private(set) var phase = Phase.idle
  /// The composer (conversation and thread) the recording or take is for.
  private(set) var maybeOwner: String?
  private(set) var elapsedMs: Int64 = 0
  private(set) var recentLevels: [Float] = []

  @ObservationIgnored private var maybeRecording: VoiceRecording?
  @ObservationIgnored private var maybePoll: Task<Void, Never>?
  @ObservationIgnored private var maybeMaxSeconds: UInt32?
  @ObservationIgnored private var hasAudioSession = false
  @ObservationIgnored private var interruptionObserver: NSObjectProtocol?

  private init() {
    // A phone call or another app taking the audio stops the recording; the take stays.
    interruptionObserver = NotificationCenter.default.addObserver(
      forName: AVAudioSession.interruptionNotification, object: nil, queue: .main
    ) { notification in
      let raw = notification.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt
      guard raw == AVAudioSession.InterruptionType.began.rawValue else { return }
      MainActor.assumeIsolated { VoiceRecorder.shared.interrupt(.call) }
    }
  }

  /// What the composer of `owner` shows.
  func phase(for owner: String) -> Phase {
    maybeOwner == owner ? phase : .idle
  }

  /// Another composer has a recording or a take: this one can't start one.
  func isBusy(elsewhereThan owner: String) -> Bool {
    maybeOwner != nil && maybeOwner != owner && phase != .idle
  }

  /// The record button: the explanation the first time, then the system's question, then the
  /// recording. `isLocked`: hands-free (a tap) instead of held.
  func begin(owner: String, maxSeconds: UInt32, isLocked: Bool) async {
    guard !isBusy(elsewhereThan: owner), phase == .idle || isRefusal else { return }
    maybeOwner = owner
    maybeMaxSeconds = maxSeconds
    switch AVAudioApplication.shared.recordPermission {
    case .granted:
      await start(maxSeconds: maxSeconds, isLocked: isLocked)
    case .denied:
      phase = .refused(.permission)
    default:
      if UserDefaults.standard.bool(forKey: Self.explainedKey) {
        await askAndStart()
      } else {
        phase = .explaining
      }
    }
  }

  /// "Continue" under the explanation: the system's question, then a hands-free recording.
  func askAndStart() async {
    UserDefaults.standard.set(true, forKey: Self.explainedKey)
    guard await AVAudioApplication.requestRecordPermission() else {
      phase = .refused(.permission)
      return
    }
    await start(maxSeconds: maybeMaxSeconds ?? VoiceMessagePolicy.fallback.maxSeconds, isLocked: true)
  }

  /// A held recording goes on hands-free.
  func lock() {
    switch phase {
    case .starting: phase = .starting(isLocked: true)
    case .recording: phase = .recording(isLocked: true)
    default: break
    }
  }

  /// A held recording (or its microphone opening) shows in place of the text field, beside the
  /// button the finger holds.
  func isHeld(by owner: String) -> Bool {
    switch phase(for: owner) {
    case .starting(isLocked: false), .recording(isLocked: false): return true
    default: return false
    }
  }

  /// The composer shows the recording bar instead of its row.
  func showsBar(for owner: String) -> Bool {
    switch phase(for: owner) {
    case .idle, .starting(isLocked: false), .recording(isLocked: false): return false
    default: return true
    }
  }

  /// Stops the recording and shows the take. `reason`: why the app stopped it, when the user didn't.
  func finish(reason: VoiceStop? = nil) async {
    guard case .recording = phase, let recording = maybeRecording else { return }
    phase = .finishing
    stopPolling()
    maybeRecording = nil
    let result = await Task.detached { () -> Result<VoiceTakeInfo, Error> in
      Result { try recording.finish(maybeReason: reason) }
    }.value
    endAudioSession()
    switch result {
    case .success(let take):
      phase = .review(
        VoiceTake(
          path: take.path, durationMs: Int64(take.durationMs), waveform: [UInt8](take.waveform),
          maybeStop: take.maybeStop))
    case .failure:
      // Nothing recorded (stopped at once): nothing to show.
      reset()
    }
  }

  /// The app stopped a recording (a call, the background, the lock, leaving the conversation).
  func interrupt(_ reason: VoiceStop, owner maybeOnlyOwner: String? = nil) {
    if let owner = maybeOnlyOwner, owner != maybeOwner { return }
    switch phase {
    case .recording:
      Task { await finish(reason: reason) }
    case .starting:
      // The microphone is still opening: nothing recorded yet, nothing to keep.
      discard()
    case .explaining, .refused:
      // Nothing recorded yet: the question goes.
      if reason == .left { reset() }
    default:
      break
    }
  }

  /// Discards the recording or the take.
  func discard() {
    stopPolling()
    if let recording = maybeRecording {
      maybeRecording = nil
      Task.detached { recording.cancel() }
    }
    if case .review(let take) = phase { try? FileManager.default.removeItem(atPath: take.path) }
    endAudioSession()
    reset()
  }

  /// The take of `owner` as a file to send: finished first if still recording, then moved into the
  /// uploads folder (the core reads it there while it encrypts and uploads it). `nil` when there is
  /// none.
  func takeForSending(owner: String, importer: AttachmentImporter) async throws -> PickedFile? {
    guard maybeOwner == owner else { return nil }
    if case .recording = phase { await finish() }
    guard case .review(let take) = phase else { return nil }
    let source = URL(fileURLWithPath: take.path)
    let folder = VoiceFolders.takes.appendingPathComponent(UUID().uuidString, isDirectory: true)
    let named = folder.appendingPathComponent(Self.fileName(at: Date()))
    defer { try? FileManager.default.removeItem(at: folder) }
    do {
      try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
      try FileManager.default.moveItem(at: source, to: named)
    } catch {
      throw AttachmentError.unreadable
    }
    reset()
    let picked = try importer.importPickedMedia(PickedMedia(url: named))
    return PickedFile(
      fileName: picked.fileName, mediaType: "audio/ogg", path: picked.path, maybeDurationMs: take.durationMs,
      maybeWaveform: take.waveform)
  }

  static func fileName(at date: Date) -> String {
    let parts = Calendar.current.dateComponents([.year, .month, .day, .hour, .minute, .second], from: date)
    return voiceFileName(
      year: Int32(parts.year ?? 0), month: UInt32(parts.month ?? 0), day: UInt32(parts.day ?? 0),
      hour: UInt32(parts.hour ?? 0), minute: UInt32(parts.minute ?? 0), second: UInt32(parts.second ?? 0))
  }

  private var isRefusal: Bool {
    if case .refused = phase { return true }
    return false
  }

  private func start(maxSeconds: UInt32, isLocked: Bool) async {
    // Never during a call: the call owns the microphone and the audio session.
    guard AppModel.shared.calls.activeCall == nil else {
      reset()
      return
    }
    VoicePlayer.shared.pause()
    let session = AVAudioSession.sharedInstance()
    do {
      try session.setCategory(.playAndRecord, mode: .default, options: [.defaultToSpeaker, .allowBluetooth])
      try session.setActive(true)
      hasAudioSession = true
    } catch {
      phase = .refused(.failed)
      return
    }
    phase = .starting(isLocked: isLocked)
    let path = VoiceFolders.takes.appendingPathComponent("take-\(UUID().uuidString).ogg").path
    let result = await Task.detached { () -> Result<VoiceRecording, Error> in
      Result { try VoiceRecording.start(path: path, maxSeconds: maxSeconds) }
    }.value
    switch result {
    case .success(let recording):
      // Discarded or interrupted while the microphone opened.
      guard case .starting(let isLockedNow) = phase else {
        Task.detached { recording.cancel() }
        return
      }
      maybeRecording = recording
      elapsedMs = 0
      recentLevels = []
      phase = .recording(isLocked: isLockedNow)
      startPolling(recording)
    case .failure:
      endAudioSession()
      phase = .refused(.failed)
    }
  }

  /// The level and length ten times a second while recording; a recording that stopped by itself
  /// (the maximum length, the microphone gone) is finished and shown.
  private func startPolling(_ recording: VoiceRecording) {
    maybePoll = Task { [weak self] in
      while !Task.isCancelled {
        guard let self else { return }
        elapsedMs = Int64(recording.elapsedMs())
        recentLevels.append(recording.level())
        if recentLevels.count > Self.shownLevels { recentLevels.removeFirst(recentLevels.count - Self.shownLevels) }
        if recording.stoppedByItself() != nil {
          await finish()
          return
        }
        try? await Task.sleep(for: .milliseconds(100))
      }
    }
  }

  private func stopPolling() {
    maybePoll?.cancel()
    maybePoll = nil
  }

  private func endAudioSession() {
    guard hasAudioSession else { return }
    hasAudioSession = false
    // A call that started meanwhile owns the session now.
    guard AppModel.shared.calls.activeCall == nil else { return }
    try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
  }

  private func reset() {
    phase = .idle
    maybeOwner = nil
    elapsedMs = 0
    recentLevels = []
  }
}

/// The app's one voice message player: one plays at a time, and it pauses for a recording, a
/// call, the background and the lock.
@MainActor
@Observable
final class VoicePlayer {
  static let shared = VoicePlayer()
  /// Received messages are decoded at most this long (the longest a workspace allows, and a minute).
  static let maxDecodedSeconds: UInt32 = 61 * 60

  enum Failure: Equatable {
    /// Not downloaded (offline, server trouble): worth trying again.
    case unavailable
    /// Here, but not playable on this device.
    case unplayable
  }

  enum State: Equatable {
    case idle
    case loading
    case playing
    case paused
    case failed(Failure)
  }

  /// The file the player holds (its message and file id).
  private(set) var maybeKey: String?
  private(set) var state = State.idle
  private(set) var positionMs: Int64 = 0
  private(set) var durationMs: Int64 = 0
  private(set) var speed = VoiceSpeed.stored(UserDefaults.standard.string(forKey: VoiceSpeed.key) ?? "")

  @ObservationIgnored private var maybePlayer: AVAudioPlayer?
  @ObservationIgnored private var maybeDecoded: URL?
  @ObservationIgnored private var maybeLoad: Task<Void, Never>?
  @ObservationIgnored private var maybeTick: Task<Void, Never>?
  @ObservationIgnored private var hasAudioSession = false
  @ObservationIgnored private var interruptionObserver: NSObjectProtocol?

  private init() {
    interruptionObserver = NotificationCenter.default.addObserver(
      forName: AVAudioSession.interruptionNotification, object: nil, queue: .main
    ) { notification in
      let raw = notification.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt
      guard raw == AVAudioSession.InterruptionType.began.rawValue else { return }
      MainActor.assumeIsolated { VoicePlayer.shared.pause() }
    }
  }

  static func key(of file: FileInfo) -> String { "\(file.message)/\(file.fileId)" }

  /// Where the player stands with `file`; `.idle` while it holds another or none.
  func state(of file: FileInfo) -> State {
    maybeKey == Self.key(of: file) ? state : .idle
  }

  /// Play or pause `file`; another file stops the one before. `startFraction`: where to start
  /// (0...1) when it isn't loaded yet.
  func toggle(_ file: FileInfo, model: AppModel, startFraction: Double = 0) {
    let key = Self.key(of: file)
    if maybeKey == key {
      switch state {
      case .playing: pause()
      case .paused: resume()
      case .loading: break
      case .idle, .failed: load(file, model: model, startFraction: startFraction)
      }
    } else {
      load(file, model: model, startFraction: startFraction)
    }
  }

  /// Seeks within the held file, or starts `file` there.
  func seek(_ file: FileInfo, to fraction: Double, model: AppModel) {
    let fraction = min(max(fraction, 0), 1)
    guard maybeKey == Self.key(of: file), let player = maybePlayer else {
      load(file, model: model, startFraction: fraction)
      return
    }
    player.currentTime = player.duration * fraction
    positionMs = Int64(player.currentTime * 1000)
  }

  /// Seeks by `seconds` within the held file (VoiceOver's adjustable action).
  func skip(_ file: FileInfo, by seconds: Double) {
    guard maybeKey == Self.key(of: file), let player = maybePlayer else { return }
    player.currentTime = min(max(player.currentTime + seconds, 0), player.duration)
    positionMs = Int64(player.currentTime * 1000)
  }

  func cycleSpeed() {
    speed = speed.next
    UserDefaults.standard.set(speed.rawValue, forKey: VoiceSpeed.key)
    maybePlayer?.rate = speed.rate
  }

  func pause() {
    guard state == .playing else { return }
    maybePlayer?.pause()
    state = .paused
    stopTicking()
    endAudioSession()
  }

  /// Lets go of the file: playback ends and the decoded copy is deleted.
  func stop() {
    maybeLoad?.cancel()
    maybeLoad = nil
    stopTicking()
    maybePlayer?.stop()
    maybePlayer = nil
    if let decoded = maybeDecoded { try? FileManager.default.removeItem(at: decoded) }
    maybeDecoded = nil
    maybeKey = nil
    state = .idle
    positionMs = 0
    durationMs = 0
    endAudioSession()
  }

  private func load(_ file: FileInfo, model: AppModel, startFraction: Double) {
    stop()
    // The recording goes first; a call owns the audio.
    guard model.calls.activeCall == nil, VoiceRecorder.shared.phase == .idle || isReviewing else { return }
    maybeKey = Self.key(of: file)
    durationMs = file.maybeDurationMs ?? 0
    state = .loading
    maybeLoad = Task { [weak self] in
      guard let path = await Self.localPath(of: file, model: model) else {
        self?.failIfCurrent(file, .unavailable)
        return
      }
      guard !Task.isCancelled else { return }
      DownloadUse.mark(path)
      let prepared = await Self.playable(path: path, mediaType: file.mediaType, fileName: file.fileName)
      guard let self, !Task.isCancelled, maybeKey == Self.key(of: file) else {
        if let decoded = prepared.maybeDecoded { try? FileManager.default.removeItem(at: decoded) }
        return
      }
      maybeDecoded = prepared.maybeDecoded
      guard let url = prepared.maybeURL, let player = try? AVAudioPlayer(contentsOf: url) else {
        failIfCurrent(file, .unplayable)
        return
      }
      player.enableRate = true
      player.rate = speed.rate
      player.prepareToPlay()
      player.currentTime = player.duration * startFraction
      maybePlayer = player
      durationMs = Int64(player.duration * 1000)
      positionMs = Int64(player.currentTime * 1000)
      state = .paused
      resume()
    }
  }

  private var isReviewing: Bool {
    if case .review = VoiceRecorder.shared.phase { return true }
    return false
  }

  private func resume() {
    guard let player = maybePlayer, AppModel.shared.calls.activeCall == nil else { return }
    startAudioSession()
    player.rate = speed.rate
    if player.currentTime >= player.duration - 0.05 { player.currentTime = 0 }
    guard player.play() else {
      state = .failed(.unplayable)
      return
    }
    state = .playing
    startTicking(player)
  }

  /// The position for the waveform while it plays; the end pauses at the start again.
  private func startTicking(_ player: AVAudioPlayer) {
    stopTicking()
    maybeTick = Task { [weak self] in
      while !Task.isCancelled {
        guard let self else { return }
        positionMs = Int64(player.currentTime * 1000)
        if !player.isPlaying && state == .playing {
          player.currentTime = 0
          positionMs = 0
          state = .paused
          endAudioSession()
          return
        }
        try? await Task.sleep(for: .milliseconds(100))
      }
    }
  }

  private func stopTicking() {
    maybeTick?.cancel()
    maybeTick = nil
  }

  private func failIfCurrent(_ file: FileInfo, _ failure: Failure) {
    guard maybeKey == Self.key(of: file) else { return }
    state = .failed(failure)
  }

  /// Sound also with the silent switch on, as for a message the user pressed play on. A running
  /// call keeps its session.
  private func startAudioSession() {
    guard !hasAudioSession, AppModel.shared.calls.activeCall == nil else { return }
    let session = AVAudioSession.sharedInstance()
    try? session.setCategory(.playback, mode: .spokenAudio)
    try? session.setActive(true)
    hasAudioSession = true
  }

  private func endAudioSession() {
    guard hasAudioSession else { return }
    hasAudioSession = false
    guard AppModel.shared.calls.activeCall == nil, VoiceRecorder.shared.phase == .idle || isReviewing else { return }
    try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
  }

  /// Follows the file until it is here, asking for it once (downloads report their end in the store,
  /// not every chunk, so this looks every second while one runs). `nil` once it failed or is gone.
  private static func localPath(of file: FileInfo, model: AppModel) async -> String? {
    var hasRequested = false
    while !Task.isCancelled {
      let extras = await model.extras(conversationId: file.conversation, messageIds: [file.message])
      guard let current = extras[file.message]?.files?.first(where: { $0.fileId == file.fileId }) else { return nil }
      switch current.transfer.state {
      case "local":
        if let path = current.transfer.path { return path }
      case "failed" where hasRequested:
        return nil
      case "remote", "failed":
        hasRequested = true
        await model.perform(
          .downloadAttachment(conversationId: file.conversation, messageId: file.message, fileId: file.fileId))
      default:
        break
      }
      try? await Task.sleep(for: .seconds(1))
    }
    return nil
  }

  /// What AVAudioPlayer opens: the file itself, or for Opus in Ogg or WebM a WAV copy the core
  /// decodes into the private playback folder.
  private static func playable(path: String, mediaType: String, fileName: String) async -> (
    maybeURL: URL?, maybeDecoded: URL?
  ) {
    guard VoiceFormat.needsDecoding(mediaType: mediaType, fileName: fileName) else {
      return (URL(fileURLWithPath: path), nil)
    }
    let output = VoiceFolders.playback.appendingPathComponent("\(UUID().uuidString).wav")
    let outputPath = output.path
    let maxSeconds = maxDecodedSeconds
    let decoded = await Task.detached { () -> Bool in
      (try? voiceDecodeToWav(input: path, output: outputPath, maxSeconds: maxSeconds)) != nil
    }.value
    return decoded ? (output, output) : (nil, nil)
  }
}

/// Which voice messages AVFoundation can't read and the core decodes: Opus in Ogg (the apps,
/// Firefox) or WebM (Chrome, Edge). By the message's type; a type that says nothing goes by the
/// name.
enum VoiceFormat {
  private static let decodedTypes: Set<String> = ["audio/ogg", "audio/opus", "audio/webm", "video/webm"]
  private static let decodedExtensions: Set<String> = ["ogg", "oga", "opus", "webm"]

  static func needsDecoding(mediaType: String, fileName: String) -> Bool {
    let essence =
      mediaType.split(separator: ";").first.map { $0.trimmingCharacters(in: .whitespaces).lowercased() } ?? ""
    if decodedTypes.contains(essence) { return true }
    guard essence.isEmpty || essence == "application/octet-stream" else { return false }
    return decodedExtensions.contains(URL(fileURLWithPath: fileName).pathExtension.lowercased())
  }
}

extension AppModel {
  /// Whether voice messages may be recorded and how long (the verified workspace policy).
  func voiceMessagePolicy() async -> VoiceMessagePolicy {
    guard let json = try? await core().snapshot(),
      let policy = Wire.decode(VoicePolicySnapshot.self, from: json)?.security?.voiceMessagePolicy
    else { return .fallback }
    return policy
  }
}
