import Foundation

/// The smallest crop, as a share of the picture's side: a handle can't fold the frame up.
let minCropShare: Double = 0.08

/// A frame's corner the user drags.
enum CropCorner {
  case topLeft, topRight, bottomLeft, bottomRight
}

/// A crop frame in shares of the picture's width and height (0 to 1), in the picture as it is
/// shown, so it doesn't depend on the size it is drawn at or decoded with (as on Android).
struct CropFrame: Equatable {
  var left = 0.0
  var top = 0.0
  var right = 1.0
  var bottom = 1.0

  var width: Double { right - left }
  var height: Double { bottom - top }
  var isWhole: Bool { left <= 0 && top <= 0 && right >= 1 && bottom >= 1 }

  /// The same part of the picture after it turned a quarter to the left (counter-clockwise).
  func turnedLeft() -> CropFrame {
    CropFrame(left: top, top: 1 - right, right: bottom, bottom: 1 - left)
  }

  /// The same part after a quarter to the right: undoes ``turnedLeft()``.
  func turnedRight() -> CropFrame {
    CropFrame(left: 1 - bottom, top: left, right: 1 - top, bottom: right)
  }

  /// Moved by a share of the picture, kept inside it with its size.
  func moved(dx: Double, dy: Double) -> CropFrame {
    let x = min(max(left + dx, 0), 1 - width)
    let y = min(max(top + dy, 0), 1 - height)
    return CropFrame(left: x, top: y, right: x + width, bottom: y + height)
  }

  /// One corner dragged by a share of the picture; the frame stays inside and at least ``minCropShare``.
  func dragged(_ corner: CropCorner, dx: Double, dy: Double) -> CropFrame {
    var frame = self
    switch corner {
    case .topLeft:
      frame.left = min(max(left + dx, 0), right - minCropShare)
      frame.top = min(max(top + dy, 0), bottom - minCropShare)
    case .topRight:
      frame.right = max(min(right + dx, 1), left + minCropShare)
      frame.top = min(max(top + dy, 0), bottom - minCropShare)
    case .bottomLeft:
      frame.left = min(max(left + dx, 0), right - minCropShare)
      frame.bottom = max(min(bottom + dy, 1), top + minCropShare)
    case .bottomRight:
      frame.right = max(min(right + dx, 1), left + minCropShare)
      frame.bottom = max(min(bottom + dy, 1), top + minCropShare)
    }
    return frame
  }

  /// The frame in the picture before `quarterTurnsLeft` turns to the left: where to cut the
  /// original before turning it.
  func inOriginal(quarterTurnsLeft: Int) -> CropFrame {
    var frame = self
    for _ in 0..<((quarterTurnsLeft % 4 + 4) % 4) { frame = frame.turnedRight() }
    return frame
  }
}
