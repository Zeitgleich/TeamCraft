import SwiftUI
import UIKit

/// Bars of a waveform, the played part in the accent colour.
struct WaveformBars: View {
  /// Heights from 0 to 1, one per bar.
  let heights: [Float]
  /// 0...1 of the bars drawn as played.
  var progress: Double = 0
  var playedColor: Color = Theme.accent
  var restColor: Color = Theme.textMuted.opacity(0.45)

  var body: some View {
    GeometryReader { geometry in
      let count = max(heights.count, 1)
      let spacing: CGFloat = 2
      let width = max((geometry.size.width - spacing * CGFloat(count - 1)) / CGFloat(count), 1)
      HStack(alignment: .center, spacing: spacing) {
        ForEach(Array(heights.enumerated()), id: \.offset) { index, height in
          Capsule()
            .fill(Double(index) / Double(count) < progress ? playedColor : restColor)
            .frame(width: width, height: max(3, geometry.size.height * CGFloat(height)))
        }
      }
      .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .leading)
    }
    .accessibilityHidden(true)
  }
}

/// A received or sent voice message (#204): play and pause, the waveform to scrub, the time and the
/// speed. One plays at a time through the app's `VoicePlayer`.
struct VoiceCard: View {
  @Environment(AppModel.self) private var model
  let file: FileInfo
  var isInBubble = false

  private static let bars = 36
  private var player: VoicePlayer { VoicePlayer.shared }

  var body: some View {
    VStack(alignment: .leading, spacing: 4) {
      HStack(spacing: 10) {
        playButton
        VStack(alignment: .leading, spacing: 4) {
          waveform.frame(height: 28)
          HStack {
            Text(verbatim: timeText).font(.app(.caption).monospacedDigit()).foregroundStyle(Theme.textMuted)
            Spacer(minLength: 8)
            speedButton
          }
        }
      }
      failure
      if !isOnServer { FileStateLine(file: file) }
    }
    .padding(8)
    .frame(maxWidth: 300, alignment: .leading)
    .background(
      isInBubble ? AnyShapeStyle(HierarchicalShapeStyle.primary.opacity(0.08)) : AnyShapeStyle(Theme.surface),
      in: RoundedRectangle(cornerRadius: Theme.radius))
    .accessibilityElement(children: .contain)
    .accessibilityLabel(Text("voice_message"))
  }

  private var state: VoicePlayer.State { player.state(of: file) }

  /// On the server or here: it can play. Still going up, it can't yet.
  private var isOnServer: Bool { ["local", "remote", "downloading", "failed"].contains(file.transfer.state) }

  private var heights: [Float] {
    voiceWaveformHeights(waveform: Data(file.maybeWaveform ?? []), count: UInt32(Self.bars))
  }

  private var progress: Double {
    guard state != .idle, player.durationMs > 0 else { return 0 }
    return Double(player.positionMs) / Double(player.durationMs)
  }

  private var timeText: String {
    switch state {
    case .playing, .paused:
      return "\(VideoDurationText.format(milliseconds: player.positionMs)) / "
        + VideoDurationText.format(milliseconds: player.durationMs)
    default:
      return VideoDurationText.format(milliseconds: file.maybeDurationMs ?? 0)
    }
  }

  private var playButton: some View {
    Button {
      player.toggle(file, model: model)
    } label: {
      Group {
        if state == .loading {
          ProgressView().tint(Theme.onAccent)
        } else {
          Image(systemName: state == .playing ? "pause.fill" : "play.fill").font(.app(.body).weight(.semibold))
        }
      }
      .foregroundStyle(Theme.onAccent)
      .frame(width: 44, height: 44)
      .background(Theme.accent, in: Circle())
    }
    .buttonStyle(.plain)
    .disabled(!isOnServer || state == .failed(.unplayable))
    .accessibilityLabel(Text(state == .playing ? LocalizedStringKey("voice_pause") : "voice_play"))
  }

  /// Tap or drag on the waveform to seek; VoiceOver adjusts by five seconds.
  private var waveform: some View {
    GeometryReader { geometry in
      WaveformBars(heights: heights, progress: progress)
        .contentShape(Rectangle())
        .gesture(
          DragGesture(minimumDistance: 0).onEnded { value in
            guard isOnServer, geometry.size.width > 0 else { return }
            player.seek(file, to: Double(value.location.x / geometry.size.width), model: model)
          })
    }
    .accessibilityElement()
    .accessibilityLabel(Text("voice_position"))
    .accessibilityValue(Text(verbatim: timeText))
    .accessibilityAdjustableAction { direction in
      switch direction {
      case .increment: player.skip(file, by: 5)
      case .decrement: player.skip(file, by: -5)
      @unknown default: break
      }
    }
  }

  private var speedButton: some View {
    Button {
      player.cycleSpeed()
    } label: {
      Text(verbatim: player.speed.label)
        .font(.app(.caption).weight(.semibold).monospacedDigit())
        .padding(.horizontal, 8).padding(.vertical, 4)
        .frame(minWidth: 44, minHeight: 32)
        .background(Theme.accentSoft, in: Capsule())
    }
    .buttonStyle(.plain)
    .accessibilityLabel(Text("voice_speed"))
    .accessibilityValue(Text(verbatim: player.speed.label))
  }

  @ViewBuilder
  private var failure: some View {
    switch state {
    case .failed(.unplayable):
      Text("voice_unplayable").font(.app(.caption2)).foregroundStyle(Theme.danger)
    case .failed(.unavailable):
      HStack(spacing: 8) {
        Text("voice_unavailable").font(.app(.caption2)).foregroundStyle(Theme.danger)
        Button("retry") { player.toggle(file, model: model) }.font(.app(.caption).weight(.semibold))
      }
    default:
      EmptyView()
    }
  }
}

/// The round record button of the composer, where the send button is while there is nothing to
/// send. Held, it records until let go (sends), slid left (discards) or slid up (goes on
/// hands-free); a tap records hands-free.
struct VoiceRecordButton: View {
  let owner: String
  let maxSeconds: UInt32
  let isDisabled: Bool
  /// The take is to be sent (a held recording let go).
  let onSend: () -> Void

  @State private var maybeHold: Task<Void, Never>?
  @State private var isHolding = false
  @State private var translation = CGSize.zero

  private var recorder: VoiceRecorder { VoiceRecorder.shared }

  var body: some View {
    Image(systemName: "mic.fill")
      .font(.app(.body).weight(.semibold))
      .foregroundStyle(isRecording ? Theme.onAccent : Theme.textMuted)
      .frame(width: 44, height: 44)
      .background(isRecording ? Theme.danger : Theme.surface, in: Circle())
      .overlay(Circle().stroke(isRecording ? Color.clear : Theme.border))
      .scaleEffect(isRecording ? 1.15 : 1)
      .offset(x: isHolding ? min(0, translation.width) : 0, y: isHolding ? min(0, translation.height) : 0)
      .animation(.spring(response: 0.25, dampingFraction: 0.8), value: isRecording)
      .contentShape(Circle())
      .gesture(press, including: isDisabled ? .none : .all)
      .opacity(isDisabled ? 0.4 : 1)
      .accessibilityElement()
      .accessibilityLabel(Text("voice_record"))
      .accessibilityHint(Text(isDisabled ? LocalizedStringKey("voice_busy_elsewhere") : "voice_record_hint"))
      .accessibilityAddTraits(.isButton)
      .accessibilityAction {
        guard !isDisabled else { return }
        Task { await recorder.begin(owner: owner, maxSeconds: maxSeconds, isLocked: true) }
      }
  }

  private var isRecording: Bool {
    if case .recording = recorder.phase(for: owner) { return true }
    return false
  }

  private var press: some Gesture {
    DragGesture(minimumDistance: 0)
      .onChanged { value in
        translation = value.translation
        guard maybeHold == nil, !isHolding else { return }
        maybeHold = Task {
          guard (try? await Task.sleep(for: VoiceHoldGesture.holdDelay)) != nil else { return }
          isHolding = true
          UIImpactFeedbackGenerator(style: .medium).impactOccurred()
          await recorder.begin(owner: owner, maxSeconds: maxSeconds, isLocked: false)
        }
      }
      .onEnded { value in
        maybeHold?.cancel()
        maybeHold = nil
        let wasHolding = isHolding
        isHolding = false
        translation = .zero
        guard wasHolding else {
          // A tap: hands-free.
          Task { await recorder.begin(owner: owner, maxSeconds: maxSeconds, isLocked: true) }
          return
        }
        switch recorder.phase(for: owner) {
        case .recording(isLocked: false):
          switch VoiceHoldGesture.outcome(translation: value.translation) {
          case .send: onSend()
          case .cancel: recorder.discard()
          case .lock: recorder.lock()
          }
        case .starting:
          // Let go before the microphone ran: it goes on hands-free.
          recorder.lock()
        default:
          break
        }
      }
  }
}

/// What a held recording shows in place of the text field: the time and where to slide.
struct VoiceHoldHint: View {
  let owner: String

  private var recorder: VoiceRecorder { VoiceRecorder.shared }

  var body: some View {
    HStack(spacing: 10) {
      RecordingDot()
      Text(verbatim: VideoDurationText.format(milliseconds: recorder.elapsedMs))
        .font(.app(.body).monospacedDigit())
      Spacer(minLength: 4)
      Label("voice_slide_cancel", systemImage: "chevron.left").font(.app(.footnote)).foregroundStyle(Theme.textMuted)
      Image(systemName: "lock").font(.app(.footnote)).foregroundStyle(Theme.textMuted)
        .accessibilityLabel(Text("voice_slide_lock"))
    }
    .padding(.horizontal, 12)
    .frame(minHeight: 44)
    .background(Theme.surface, in: RoundedRectangle(cornerRadius: 22, style: .continuous))
    .accessibilityElement(children: .combine)
  }
}

/// The composer row while a recording runs hands-free, is finished for review, or the microphone
/// needs explaining or was refused.
struct VoiceRecorderBar: View {
  let owner: String
  /// The take is to be sent.
  let onSend: () -> Void

  private var recorder: VoiceRecorder { VoiceRecorder.shared }

  var body: some View {
    Group {
      switch recorder.phase(for: owner) {
      case .explaining:
        notice("voice_explain") {
          Button("cancel") { recorder.discard() }
          Button("voice_continue") { Task { await recorder.askAndStart() } }.fontWeight(.semibold)
        }
      case .refused(.permission):
        notice("voice_permission_denied") {
          Button("cancel") { recorder.discard() }
          Button("voice_open_settings") {
            if let url = URL(string: UIApplication.openSettingsURLString) { UIApplication.shared.open(url) }
            recorder.discard()
          }
          .fontWeight(.semibold)
        }
      case .refused(.failed):
        notice("voice_failed") {
          Button("done") { recorder.discard() }
        }
      case .starting, .recording, .finishing:
        recordingRow
      case .review(let take):
        reviewRow(take)
      case .idle:
        EmptyView()
      }
    }
    .animation(.easeOut(duration: 0.2), value: recorder.phase(for: owner))
  }

  private var recordingRow: some View {
    HStack(spacing: 8) {
      discardButton
      RecordingDot()
      Text(verbatim: VideoDurationText.format(milliseconds: recorder.elapsedMs))
        .font(.app(.body).monospacedDigit())
        .accessibilityLabel(Text("voice_recording"))
        .accessibilityValue(Text(verbatim: VideoDurationText.format(milliseconds: recorder.elapsedMs)))
      WaveformBars(
        // The newest level on the right, as it comes in.
        heights: Array(repeating: 0, count: max(0, VoiceRecorder.shownLevels - recorder.recentLevels.count))
          + recorder.recentLevels,
        progress: 1, playedColor: Theme.danger
      )
      .frame(height: 28)
      Button {
        Task { await recorder.finish() }
      } label: {
        roundIcon("stop.fill", filled: false)
      }
      .buttonStyle(.plain)
      .disabled(recorder.phase(for: owner) == .finishing)
      .accessibilityLabel(Text("voice_stop"))
      sendButton
    }
  }

  private func reviewRow(_ take: VoiceTake) -> some View {
    VStack(alignment: .leading, spacing: 4) {
      HStack(spacing: 8) {
        discardButton
        WaveformBars(heights: voiceWaveformHeights(waveform: Data(take.waveform), count: 32), progress: 0)
          .frame(height: 28)
        Text(verbatim: VideoDurationText.format(milliseconds: take.durationMs)).font(.app(.body).monospacedDigit())
        sendButton
      }
      .accessibilityElement(children: .contain)
      .accessibilityLabel(Text("voice_take \(VideoDurationText.format(milliseconds: take.durationMs))"))
      if let stop = take.maybeStop {
        Text(Self.stopText(stop)).font(.app(.caption)).foregroundStyle(Theme.textMuted)
      }
    }
  }

  static func stopText(_ stop: VoiceStop) -> LocalizedStringKey {
    switch stop {
    case .limitReached: return "voice_stopped_limit"
    case .call: return "voice_stopped_call"
    case .background: return "voice_stopped_background"
    case .microphoneLost: return "voice_stopped_microphone"
    case .left: return "voice_stopped_left"
    }
  }

  private var discardButton: some View {
    Button {
      recorder.discard()
    } label: {
      Image(systemName: "trash").font(.app(.body)).foregroundStyle(Theme.danger).frame(width: 44, height: 44)
    }
    .buttonStyle(.plain)
    .accessibilityLabel(Text("voice_discard"))
  }

  private var canSend: Bool {
    switch recorder.phase(for: owner) {
    case .recording, .review: return true
    default: return false
    }
  }

  private var sendButton: some View {
    Button(action: onSend) { roundIcon("arrow.up", filled: true) }
      .buttonStyle(.plain)
      .disabled(!canSend)
      .accessibilityLabel(Text("send"))
  }

  private func roundIcon(_ systemName: String, filled: Bool) -> some View {
    Image(systemName: systemName)
      .font(.app(.body).weight(.semibold))
      .foregroundStyle(filled ? Theme.onAccent : Theme.text)
      .frame(width: 44, height: 44)
      .background(filled ? Theme.accent : Theme.surface, in: Circle())
      .overlay(Circle().stroke(filled ? Color.clear : Theme.border))
  }

  private func notice(_ text: LocalizedStringKey, @ViewBuilder actions: () -> some View) -> some View {
    VStack(alignment: .leading, spacing: 8) {
      Label(text, systemImage: "mic").font(.app(.subheadline))
      HStack(spacing: 16) {
        Spacer()
        actions()
      }
    }
    .padding(10)
    .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radius))
  }
}

/// The red dot of a running recording; it pulses unless motion is reduced.
struct RecordingDot: View {
  @Environment(\.accessibilityReduceMotion) private var reduceMotion
  @State private var isDim = false

  var body: some View {
    Circle()
      .fill(Theme.danger)
      .frame(width: 10, height: 10)
      .opacity(isDim ? 0.35 : 1)
      .onAppear {
        guard !reduceMotion else { return }
        withAnimation(.easeInOut(duration: 0.8).repeatForever(autoreverses: true)) { isDim = true }
      }
      .accessibilityHidden(true)
  }
}
