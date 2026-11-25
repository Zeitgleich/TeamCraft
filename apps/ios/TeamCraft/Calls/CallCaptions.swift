import AVFAudio
import Foundation
import Speech
import SwiftUI

/// Live captions of calls on this iPhone (#215, e2ee.md §8.6): the core cuts the others' voices,
/// as this device plays them, into sentences and hands each to `SFSpeechRecognizer` with
/// `requiresOnDeviceRecognition`, so no audio leaves the phone; the text shows on the call screen
/// only, and nobody in the call is told. Where the phone can't recognize its language on the
/// device, there is no captions button: recognition on Apple's servers is never used.
enum CallCaptions {
  /// The language captions recognize: the app's.
  static var locale: Locale { Locale.current }

  /// This iPhone recognizes the app's language on the device.
  static var isOffered: Bool {
    SFSpeechRecognizer(locale: locale)?.supportsOnDeviceRecognition == true
  }

  /// Asks for speech recognition once (the system's prompt); `true` when allowed.
  static func authorize() async -> Bool {
    switch SFSpeechRecognizer.authorizationStatus() {
    case .authorized: return true
    case .denied, .restricted: return false
    case .notDetermined:
      return await withCheckedContinuation { continuation in
        SFSpeechRecognizer.requestAuthorization { status in continuation.resume(returning: status == .authorized) }
      }
    @unknown default: return false
    }
  }
}

/// The system recognizer in on-device mode, for the core's captions worker. The core calls
/// `recognize` on its captions thread, one piece at a time (mono 16 kHz PCM16, at most about
/// 10 s); never on the main thread.
final class OnDeviceCaptionRecognizer: CaptionRecognizer, @unchecked Sendable {
  /// The core's rate (`call_captions.rs`).
  private static let sampleRate = 16_000.0
  /// Longer than any piece takes; beyond this the recognizer is taken as hung, and captions stop.
  private static let timeout: DispatchTimeInterval = .seconds(20)
  /// "No speech detected": nothing understood in the piece, not a failure.
  private static let noSpeechCode = 1110

  private let recognizer: SFSpeechRecognizer
  private let format: AVAudioFormat
  /// The recognizer's callbacks run here, not on the main queue.
  private let callbacks = OperationQueue()

  /// `nil` where the language can't be recognized on the device.
  init?(locale: Locale) {
    guard let recognizer = SFSpeechRecognizer(locale: locale), recognizer.supportsOnDeviceRecognition,
      let format = AVAudioFormat(
        commonFormat: .pcmFormatInt16, sampleRate: Self.sampleRate, channels: 1, interleaved: true)
    else { return nil }
    callbacks.maxConcurrentOperationCount = 1
    recognizer.queue = callbacks
    self.recognizer = recognizer
    self.format = format
  }

  func recognize(pcm16: Data) throws -> String {
    UiThreadGuard.shared.check("OnDeviceCaptionRecognizer.recognize")
    guard let buffer = buffer(of: pcm16) else { return "" }
    let request = SFSpeechAudioBufferRecognitionRequest()
    // On the device only: the audio never goes to Apple's servers (e2ee.md §8.6).
    request.requiresOnDeviceRecognition = true
    request.shouldReportPartialResults = false
    request.taskHint = .dictation
    request.addsPunctuation = true
    request.append(buffer)
    request.endAudio()
    let outcome = Outcome()
    // blocking-ok: waited on below, on the core's captions thread only.
    let done = DispatchSemaphore(value: 0)
    let task = recognizer.recognitionTask(with: request) { maybeResult, maybeError in
      if let result = maybeResult, result.isFinal {
        outcome.finish(.success(result.bestTranscription.formattedString), done)
      } else if let error = maybeError as NSError? {
        if error.code == Self.noSpeechCode {
          outcome.finish(.success(""), done)
        } else {
          // The code only: an error never carries recognized text here.
          outcome.finish(.failure(.Failed(reason: "\(error.domain) \(error.code)")), done)
        }
      }
    }
    // blocking-ok: the core's captions thread (UiThreadGuard above), never the main thread.
    if done.wait(timeout: .now() + Self.timeout) == .timedOut {
      task.cancel()
      throw CaptionRecognizeError.Failed(reason: "timed out")
    }
    return try outcome.result()
  }

  /// The piece as one buffer in the core's format.
  private func buffer(of pcm16: Data) -> AVAudioPCMBuffer? {
    let frames = AVAudioFrameCount(pcm16.count / MemoryLayout<Int16>.size)
    guard frames > 0, let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: frames),
      let samples = buffer.int16ChannelData
    else { return nil }
    buffer.frameLength = frames
    // Little-endian PCM16, as the core hands it; iPhones are little-endian.
    pcm16.withUnsafeBytes { raw in
      guard let source = raw.baseAddress else { return }
      memcpy(samples[0], source, Int(frames) * MemoryLayout<Int16>.size)
    }
    return buffer
  }

  /// The task's answer, once; later callbacks of the same task are ignored.
  private final class Outcome: @unchecked Sendable {
    private let lock = NSLock()
    private var maybeResult: Result<String, CaptionRecognizeError>?

    // blocking-ok: only signals, on the recognizer's own queue; never waits.
    func finish(_ result: Result<String, CaptionRecognizeError>, _ done: DispatchSemaphore) {
      lock.lock()
      defer { lock.unlock() }
      guard maybeResult == nil else { return }
      maybeResult = result
      done.signal()
    }

    func result() throws -> String {
      lock.lock()
      defer { lock.unlock() }
      return try (maybeResult ?? .success("")).get()
    }
  }
}

/// The captions over the stage's lower edge (#215): the last lines with the speaker's name, a
/// sentence still going on lighter. The header says the captions stay on this device. Finished
/// sentences are announced to VoiceOver by the call screen; the lines here are hidden from it.
struct CaptionsPanel: View {
  /// How many lines show.
  private static let shownLines = 3

  let lines: [CaptionLineInfo]
  let names: [String: String]

  var body: some View {
    VStack(alignment: .leading, spacing: 4) {
      Text("call_captions_only_here")
        .font(.app(.caption).weight(.medium))
        .foregroundStyle(.white.opacity(0.7))
      if lines.isEmpty {
        Text("call_captions_listening")
          .font(.app(.subheadline))
          .foregroundStyle(.white)
      }
      VStack(alignment: .leading, spacing: 4) {
        ForEach(lines.suffix(Self.shownLines), id: \.id) { line in
          (Text(verbatim: Self.name(of: line.userId, in: names)).fontWeight(.semibold)
            + Text(verbatim: "  " + line.text))
            .font(.app(.body))
            .foregroundStyle(.white.opacity(line.isFinal ? 1 : 0.7))
            .frame(maxWidth: .infinity, alignment: .leading)
        }
      }
      .accessibilityHidden(true)
    }
    .padding(.horizontal, 16)
    .padding(.vertical, 10)
    .frame(maxWidth: 560, alignment: .leading)
    .background(.black.opacity(0.7), in: RoundedRectangle(cornerRadius: 16, style: .continuous))
  }

  static func name(of userId: String, in names: [String: String]) -> String {
    if let name = names[userId], !name.isEmpty { return name }
    return String(localized: "call_captions_someone")
  }
}

/// The call screen's captions besides the panel (#215): whether this iPhone offers them, the note
/// when they went off by themselves, and each finished sentence said once to VoiceOver, politely
/// (the panel's lines are hidden from it, so parts so far aren't read).
struct CaptionsAnnouncements: ViewModifier {
  let calls: CallManager
  let lines: [CaptionLineInfo]
  @Binding var isSupported: Bool
  @Environment(\.accessibilityVoiceOverEnabled) private var isVoiceOverOn
  /// The newest finished sentence said to VoiceOver.
  @State private var maybeLastSaid: UInt64?

  func body(content: Content) -> some View {
    content
      .alert("call_captions", isPresented: noteBinding, presenting: calls.maybeCaptionsNote) { _ in
        Button("done", role: .cancel) {}
      } message: { note in
        Text(note)
      }
      .task { isSupported = CallCaptions.isOffered }
      .onChange(of: lines) { _, lines in say(lines) }
  }

  private var noteBinding: Binding<Bool> {
    Binding(get: { calls.maybeCaptionsNote != nil }, set: { if !$0 { calls.dismissCaptionsNote() } })
  }

  private func say(_ lines: [CaptionLineInfo]) {
    let finals = lines.filter { line in line.isFinal && maybeLastSaid.map { line.id > $0 } ?? true }
    guard let newest = finals.map(\.id).max() else { return }
    maybeLastSaid = newest
    guard isVoiceOverOn else { return }
    for line in finals {
      let name = CaptionsPanel.name(of: line.userId, in: calls.names)
      AccessibilityNotification.Announcement(String(localized: "call_captions_announce \(name) \(line.text)")).post()
    }
  }
}
