import AVFoundation
import SwiftUI
import UIKit

/// A Live Photo or Motion Photo over its picture (#2181): the "Live" badge, and while it plays the
/// motion muted and looping over the picture. The badge plays or stops it; `isHeld` plays it while
/// a finger stays down (the viewer's long press). The clip is downloaded only once it is to play,
/// nothing plays by itself (so Reduce Motion needs nothing more), and a clip the phone can't decode
/// leaves the picture still without the badge.
struct LiveMotionOverlay: View {
  @Environment(AppModel.self) private var model
  @Environment(\.scenePhase) private var scenePhase
  let file: FileInfo
  var isHeld = false
  var badgeAlignment: Alignment = .topLeading
  /// The clip fills the tile (cropped like the picture), or fits the viewer's screen.
  var isFilling = true

  @State private var isRequested = false
  @State private var isUnplayable = false

  private var motion: MotionInfo? { file.isLive ? file.motion : nil }
  private var wantsToPlay: Bool { (isRequested || isHeld) && !isUnplayable && scenePhase == .active }

  var body: some View {
    if let motion {
      ZStack(alignment: badgeAlignment) {
        if wantsToPlay, let path = motion.localPath {
          MotionLoopView(url: URL(fileURLWithPath: path), isFilling: isFilling, onUnplayable: { isUnplayable = true })
            .allowsHitTesting(false)
            .accessibilityHidden(true)
        } else {
          Color.clear.allowsHitTesting(false)
        }
        if !isUnplayable {
          LiveBadge(isPlaying: wantsToPlay && motion.localPath != nil, isComing: wantsToPlay && motion.transfer.isInProgress) {
            isRequested.toggle()
          }
          .padding(6)
        }
      }
      .task(id: wantsToPlay) { await fetchIfWanted(motion) }
    }
  }

  private func fetchIfWanted(_ motion: MotionInfo) async {
    guard wantsToPlay, motion.transfer.state == "remote" || motion.transfer.state == "failed" else { return }
    await model.perform(.downloadAttachment(conversationId: file.conversation, messageId: file.message, fileId: motion.fileId))
  }
}

/// Says the picture moves, and plays or stops its motion.
struct LiveBadge: View {
  let isPlaying: Bool
  let isComing: Bool
  let action: () -> Void

  var body: some View {
    Button(action: action) {
      HStack(spacing: 4) {
        if isComing {
          ProgressView().controlSize(.mini).tint(.white)
        } else {
          Image(systemName: "livephoto").font(.caption.weight(.semibold))
        }
        Text("live_badge").font(.caption2.weight(.semibold)).textCase(.uppercase).tracking(0.6)
      }
      .foregroundStyle(.white)
      .padding(.horizontal, 8)
      .padding(.vertical, 4)
      .background(.ultraThinMaterial.opacity(isPlaying ? 1 : 0.85), in: Capsule())
      .environment(\.colorScheme, .dark)
      .frame(minHeight: 44)
      .contentShape(Rectangle())
    }
    .buttonStyle(.plain)
    .accessibilityLabel(Text(isPlaying ? "live_stop" : "live_play"))
  }
}

/// A clip that plays muted and loops, filling its frame (AVPlayerLooper), from the decrypted file in
/// the app's private downloads. A clip the phone can't play calls `onUnplayable`.
struct MotionLoopView: UIViewRepresentable {
  let url: URL
  var isFilling = true
  let onUnplayable: () -> Void

  func makeUIView(context: Context) -> LoopingPlayerView {
    let view = LoopingPlayerView()
    view.isFilling = isFilling
    view.play(url, onUnplayable: onUnplayable)
    return view
  }

  func updateUIView(_ view: LoopingPlayerView, context: Context) {
    if view.maybeURL != url { view.play(url, onUnplayable: onUnplayable) }
  }

  static func dismantleUIView(_ view: LoopingPlayerView, coordinator: ()) {
    view.stop()
  }
}

final class LoopingPlayerView: UIView {
  override static var layerClass: AnyClass { AVPlayerLayer.self }
  private var maybePlayerLayer: AVPlayerLayer? { layer as? AVPlayerLayer }
  private var maybeLooper: AVPlayerLooper?
  private var maybeStatus: NSKeyValueObservation?
  private(set) var maybeURL: URL?
  var isFilling = true

  func play(_ url: URL, onUnplayable: @escaping () -> Void) {
    stop()
    maybeURL = url
    // A hostile file may not make the phone load an address (e2ee.md §4.5, as the video player).
    let asset = AVURLAsset(
      url: url, options: [AVURLAssetReferenceRestrictionsKey: AVAssetReferenceRestrictions.forbidAll.rawValue])
    let item = AVPlayerItem(asset: asset)
    let player = AVQueuePlayer()
    player.isMuted = true
    player.preventsDisplaySleepDuringVideoPlayback = false
    maybeLooper = AVPlayerLooper(player: player, templateItem: item)
    maybeStatus = item.observe(\.status) { item, _ in
      if item.status == .failed { DispatchQueue.main.async { onUnplayable() } }
    }
    maybePlayerLayer?.videoGravity = isFilling ? .resizeAspectFill : .resizeAspect
    maybePlayerLayer?.player = player
    player.play()
  }

  func stop() {
    maybePlayerLayer?.player?.pause()
    maybePlayerLayer?.player = nil
    maybeLooper = nil
    maybeStatus = nil
    maybeURL = nil
  }
}
