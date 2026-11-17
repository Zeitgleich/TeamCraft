import CoreGraphics

/// The square of a picture chosen as a profile picture (issue #2071): the picture moves and zooms
/// under a fixed circle. At zoom 1 the picture's shorter side fills the circle; the square always
/// stays inside the picture. Coordinates are pixels of the upright picture. The Android app has the
/// same geometry (`AvatarCrop.kt`).
struct AvatarCrop: Equatable {
  static let minSidePx: CGFloat = 96
  static let maxZoomLimit: CGFloat = 8

  let pictureWidth: CGFloat
  let pictureHeight: CGFloat
  private(set) var zoom: CGFloat = 1
  private(set) var centerX: CGFloat
  private(set) var centerY: CGFloat

  init(pictureWidth: CGFloat, pictureHeight: CGFloat) {
    self.pictureWidth = pictureWidth
    self.pictureHeight = pictureHeight
    centerX = pictureWidth / 2
    centerY = pictureHeight / 2
  }

  /// Side of the chosen square in picture pixels.
  var side: CGFloat { shorterSide / zoom }
  var left: CGFloat { centerX - side / 2 }
  var top: CGFloat { centerY - side / 2 }

  /// The most zoom: the square doesn't get smaller than `minSidePx` (further zoom only shows blur).
  var maxZoom: CGFloat { min(max(shorterSide / Self.minSidePx, 1), Self.maxZoomLimit) }

  private var shorterSide: CGFloat { min(pictureWidth, pictureHeight) }

  /// Moved by a drag of (`dx`, `dy`) over a circle `viewSide` wide: the picture follows the finger.
  func dragged(dx: CGFloat, dy: CGFloat, viewSide: CGFloat) -> AvatarCrop {
    guard viewSide > 0 else { return self }
    var moved = self
    let picturePerPoint = side / viewSide
    moved.centerX -= dx * picturePerPoint
    moved.centerY -= dy * picturePerPoint
    return moved.clamped()
  }

  /// Zoomed to `newZoom` around the circle's middle, within 1 and `maxZoom`.
  func zoomed(to newZoom: CGFloat) -> AvatarCrop {
    var zoomed = self
    zoomed.zoom = min(max(newZoom, 1), maxZoom)
    return zoomed.clamped()
  }

  /// The square in whole pixels, inside the picture.
  var square: CGRect {
    let whole = min(max(side.rounded(), 1), shorterSide.rounded(.down))
    let x = min(max(left.rounded(), 0), pictureWidth.rounded(.down) - whole)
    let y = min(max(top.rounded(), 0), pictureHeight.rounded(.down) - whole)
    return CGRect(x: x, y: y, width: whole, height: whole)
  }

  private func clamped() -> AvatarCrop {
    var clamped = self
    let half = side / 2
    clamped.centerX = min(max(centerX, half), pictureWidth - half)
    clamped.centerY = min(max(centerY, half), pictureHeight - half)
    return clamped
  }
}
