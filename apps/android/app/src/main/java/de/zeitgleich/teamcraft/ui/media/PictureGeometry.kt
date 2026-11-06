package de.zeitgleich.teamcraft.ui.media

import kotlin.math.abs
import kotlin.math.cos
import kotlin.math.sin

/** How far the straightening dial turns either way, in degrees. */
const val MAX_STRAIGHTEN_DEGREES = 45f

/**
 * The geometry of a crop, without Android types so it can be tested on its own: the size of the
 * turned picture and how much a straightened picture is zoomed in.
 */
object PictureGeometry {
  /** Width and height after the quarter turns. */
  fun turnedSize(width: Float, height: Float, quarterTurnsLeft: Int): Pair<Float, Float> =
    if (quarterTurnsLeft.mod(2) == 1) height to width else width to height

  /**
   * How much a picture of this size, turned by `degrees`, must be zoomed so it still covers its own
   * unturned rectangle: no empty corners after straightening, as in the common photo editors.
   */
  fun coverScale(width: Float, height: Float, degrees: Float): Float {
    if (degrees == 0f || width <= 0f || height <= 0f) return 1f
    val radians = Math.toRadians(degrees.toDouble())
    val c = abs(cos(radians)).toFloat()
    val s = abs(sin(radians)).toFloat()
    // The rectangle turned back by the angle must fit inside the zoomed picture.
    return maxOf((width * c + height * s) / width, (width * s + height * c) / height)
  }

  /** The output's size in source pixels: the frame of the turned picture. */
  fun outputSize(width: Float, height: Float, crop: CropEdit): Pair<Float, Float> {
    val (turnedWidth, turnedHeight) = turnedSize(width, height, crop.quarterTurnsLeft)
    return turnedWidth * crop.frame.width to turnedHeight * crop.frame.height
  }

  /** The scale that keeps the longer side of a `width` × `height` output at `maxSidePx` at most. */
  fun downscale(width: Float, height: Float, maxSidePx: Int): Float = minOf(1f, maxSidePx / maxOf(width, height, 1f))
}
