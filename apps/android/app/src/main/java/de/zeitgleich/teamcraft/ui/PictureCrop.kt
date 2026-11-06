package de.zeitgleich.teamcraft.ui

import kotlin.math.abs
import kotlinx.serialization.Serializable

/** The smallest crop, as a share of the picture's side: a handle can't fold the frame up. */
const val MIN_CROP_SHARE = 0.08f

/**
 * A crop frame in shares of the picture's width and height (0 to 1), in the picture as it is shown,
 * so it doesn't depend on the size it is drawn at or decoded with.
 */
@Serializable
data class CropFrame(val left: Float = 0f, val top: Float = 0f, val right: Float = 1f, val bottom: Float = 1f) {
  val width: Float get() = right - left
  val height: Float get() = bottom - top
  val isWhole: Boolean get() = left <= 0f && top <= 0f && right >= 1f && bottom >= 1f

  /** The same part of the picture after it turned a quarter to the left (counter-clockwise). */
  fun turnedLeft(): CropFrame = CropFrame(left = top, top = 1f - right, right = bottom, bottom = 1f - left)

  /** The same part after a quarter to the right: undoes [turnedLeft]. */
  fun turnedRight(): CropFrame = CropFrame(left = 1f - bottom, top = left, right = 1f - top, bottom = right)

  /** The same part after the picture was mirrored left to right. */
  fun mirrored(): CropFrame = CropFrame(left = 1f - right, top = top, right = 1f - left, bottom = bottom)

  /** Moved by a share of the picture, kept inside it with its size. */
  fun moved(dx: Float, dy: Float): CropFrame {
    val x = (left + dx).coerceIn(0f, 1f - width)
    val y = (top + dy).coerceIn(0f, 1f - height)
    return CropFrame(x, y, x + width, y + height)
  }

  /** One corner dragged by a share of the picture; the frame stays inside and at least [MIN_CROP_SHARE]. */
  fun dragged(corner: CropCorner, dx: Float, dy: Float): CropFrame = when (corner) {
    CropCorner.TopLeft -> copy(left = (left + dx).coerceIn(0f, right - MIN_CROP_SHARE), top = (top + dy).coerceIn(0f, bottom - MIN_CROP_SHARE))
    CropCorner.TopRight -> copy(right = (right + dx).coerceIn(left + MIN_CROP_SHARE, 1f), top = (top + dy).coerceIn(0f, bottom - MIN_CROP_SHARE))
    CropCorner.BottomLeft -> copy(left = (left + dx).coerceIn(0f, right - MIN_CROP_SHARE), bottom = (bottom + dy).coerceIn(top + MIN_CROP_SHARE, 1f))
    CropCorner.BottomRight -> copy(right = (right + dx).coerceIn(left + MIN_CROP_SHARE, 1f), bottom = (bottom + dy).coerceIn(top + MIN_CROP_SHARE, 1f))
  }

  /**
   * One corner dragged with the frame's shape kept: `aspect` is the frame's width over height in
   * shares (its pixel ratio divided by the picture's). The horizontal move leads; the frame stays
   * inside the picture and at least [MIN_CROP_SHARE] on its shorter side.
   */
  fun draggedLocked(corner: CropCorner, dx: Float, dy: Float, aspect: Float): CropFrame {
    val growsRight = corner == CropCorner.TopRight || corner == CropCorner.BottomRight
    val growsDown = corner == CropCorner.BottomLeft || corner == CropCorner.BottomRight
    // The fixed corner and the room from it to the picture's edges in the drag's direction.
    val anchorX = if (growsRight) left else right
    val anchorY = if (growsDown) top else bottom
    val roomX = if (growsRight) 1f - anchorX else anchorX
    val roomY = if (growsDown) 1f - anchorY else anchorY
    val dragX = if (growsRight) dx else -dx
    val dragY = if (growsDown) dy else -dy
    // The larger of the two moves, measured in width, so both directions resize.
    val wanted = width + if (abs(dragX) >= abs(dragY * aspect)) dragX else dragY * aspect
    val largest = minOf(roomX, roomY * aspect)
    val smallest = minOf(maxOf(MIN_CROP_SHARE, MIN_CROP_SHARE * aspect), largest)
    val newWidth = wanted.coerceIn(smallest, largest)
    val newHeight = newWidth / aspect
    val newLeft = if (growsRight) anchorX else anchorX - newWidth
    val newTop = if (growsDown) anchorY else anchorY - newHeight
    return CropFrame(newLeft, newTop, newLeft + newWidth, newTop + newHeight)
  }

  /**
   * The frame in the picture as it was before [quarterTurnsLeft] turns to the left: where to cut
   * the original before turning it, so the full-size picture is never turned whole.
   */
  fun inOriginal(quarterTurnsLeft: Int): CropFrame {
    var frame = this
    repeat(quarterTurnsLeft.mod(4)) { frame = frame.turnedRight() }
    return frame
  }
}

enum class CropCorner { TopLeft, TopRight, BottomLeft, BottomRight }
