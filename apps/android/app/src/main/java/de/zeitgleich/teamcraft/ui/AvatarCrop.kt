package de.zeitgleich.teamcraft.ui

import kotlin.math.roundToInt

/**
 * The square of a picture chosen as a profile picture (issue #2071): the picture moves and zooms
 * under a fixed circle. At zoom 1 the picture's shorter side fills the circle; the square always
 * stays inside the picture. Coordinates are pixels of the picture as shown (upright).
 */
data class AvatarCrop(
  val pictureWidth: Int,
  val pictureHeight: Int,
  val zoom: Float = 1f,
  val centerX: Float = pictureWidth / 2f,
  val centerY: Float = pictureHeight / 2f,
) {
  /** Side of the chosen square in picture pixels. */
  val side: Float
    get() = shorterSide / zoom

  val left: Float
    get() = centerX - side / 2

  val top: Float
    get() = centerY - side / 2

  /** The most zoom: the square doesn't get smaller than [MIN_SIDE_PX] (further zoom only shows blur). */
  val maxZoom: Float
    get() = (shorterSide / MIN_SIDE_PX).coerceIn(1f, MAX_ZOOM)

  private val shorterSide: Float
    get() = minOf(pictureWidth, pictureHeight).toFloat()

  /** Moved by a drag of ([dx], [dy]) over a circle [viewSide] wide: the picture follows the finger. */
  fun dragged(dx: Float, dy: Float, viewSide: Float): AvatarCrop {
    if (viewSide <= 0f) return this
    val picturePerViewPx = side / viewSide
    return copy(centerX = centerX - dx * picturePerViewPx, centerY = centerY - dy * picturePerViewPx).clamped()
  }

  /** Zoomed to [newZoom] around the circle's middle, within 1 and [maxZoom]. */
  fun withZoom(newZoom: Float): AvatarCrop = copy(zoom = newZoom.coerceIn(1f, maxZoom)).clamped()

  /** The square in whole pixels, inside the picture. */
  fun square(): CropSquare {
    val whole = side.roundToInt().coerceIn(1, minOf(pictureWidth, pictureHeight))
    return CropSquare(
      left = left.roundToInt().coerceIn(0, pictureWidth - whole),
      top = top.roundToInt().coerceIn(0, pictureHeight - whole),
      side = whole,
    )
  }

  private fun clamped(): AvatarCrop {
    val half = side / 2
    return copy(
      centerX = centerX.coerceIn(half, pictureWidth - half),
      centerY = centerY.coerceIn(half, pictureHeight - half),
    )
  }

  companion object {
    const val MIN_SIDE_PX = 96f
    const val MAX_ZOOM = 8f
  }
}

/** A square of a picture in whole pixels. */
data class CropSquare(val left: Int, val top: Int, val side: Int)
