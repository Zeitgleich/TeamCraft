import QuartzCore

/// Counts the frames the screen draws while a measure runs (feed.scroll_back, feed.jump_bottom,
/// issue #2180): a display link only between `start` and `stop`, never while nothing is measured.
/// A main thread busy for a while shows as one long frame.
@MainActor
final class FrameWatch: NSObject {
  private var maybeLink: CADisplayLink?
  private var maybeLastTimestamp: CFTimeInterval?
  private(set) var stats = FrameStats()

  var isRunning: Bool { maybeLink != nil }

  func start() {
    stop()
    stats = FrameStats()
    maybeLastTimestamp = nil
    let link = CADisplayLink(target: self, selector: #selector(tick(_:)))
    link.add(to: .main, forMode: .common)
    maybeLink = link
  }

  /// Stops counting; the link holds the watch until then.
  @discardableResult
  func stop() -> FrameStats {
    maybeLink?.invalidate()
    maybeLink = nil
    return stats
  }

  @objc private func tick(_ link: CADisplayLink) {
    if let last = maybeLastTimestamp { stats.add(frameMs: (link.timestamp - last) * 1_000) }
    maybeLastTimestamp = link.timestamp
  }
}
