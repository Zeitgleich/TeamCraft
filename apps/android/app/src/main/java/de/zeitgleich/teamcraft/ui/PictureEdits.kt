package de.zeitgleich.teamcraft.ui

import android.graphics.Bitmap
import android.graphics.Matrix
import java.io.ByteArrayOutputStream
import kotlin.math.roundToInt
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/**
 * Applies a crop and quarter turns to a picture before it is sent. The result is a new picture:
 * its metadata (camera, place) isn't carried over, which is what a sent photo should do anyway.
 */
object PictureEdits {
  /**
   * Longest side of an edited picture: a phone photo stays sharp in the viewer, and decoding it
   * stays within a slow phone's memory (about 36 MB at this size).
   */
  const val MAX_SIDE_PX = 3072
  private const val JPEG_QUALITY = 90

  /** Media types the editor opens; animations and vector formats are sent as they are. */
  fun isEditable(mediaType: String): Boolean = mediaType in EDITABLE_TYPES

  /** The edited picture's bytes and media type (PNG stays PNG for its transparency, the rest is JPEG). */
  suspend fun render(path: String, mediaType: String, quarterTurnsLeft: Int, frame: CropFrame): Pair<ByteArray, String>? =
    withContext(Dispatchers.Default) {
      val source = Pictures.decodeUpright(path, MAX_SIDE_PX) ?: return@withContext null
      try {
        val original = frame.inOriginal(quarterTurnsLeft)
        val x = (original.left * source.width).roundToInt().coerceIn(0, source.width - 1)
        val y = (original.top * source.height).roundToInt().coerceIn(0, source.height - 1)
        val width = (original.width * source.width).roundToInt().coerceIn(1, source.width - x)
        val height = (original.height * source.height).roundToInt().coerceIn(1, source.height - y)
        val turn = Matrix().apply { postRotate(-QUARTER * quarterTurnsLeft.mod(4)) }
        val edited = Bitmap.createBitmap(source, x, y, width, height, turn, true)
        val isPng = mediaType == "image/png"
        val bytes = ByteArrayOutputStream().use { out ->
          edited.compress(if (isPng) Bitmap.CompressFormat.PNG else Bitmap.CompressFormat.JPEG, JPEG_QUALITY, out)
          out.toByteArray()
        }
        if (edited !== source) edited.recycle()
        bytes to (if (isPng) "image/png" else "image/jpeg")
      } finally {
        source.recycle()
      }
    }

  /** The preview turned by quarter turns, for showing while editing. */
  fun turned(preview: Bitmap, quarterTurnsLeft: Int): Bitmap {
    val turns = quarterTurnsLeft.mod(4)
    if (turns == 0) return preview
    val turn = Matrix().apply { postRotate(-QUARTER * turns) }
    return Bitmap.createBitmap(preview, 0, 0, preview.width, preview.height, turn, true)
  }

  /** The file name of the edited picture: a JPEG gets the `.jpg` ending. */
  fun editedName(name: String, mediaType: String): String {
    if (mediaType == "image/png") return name
    val base = name.substringBeforeLast('.', name)
    return "$base.jpg"
  }

  private const val QUARTER = 90f
  private val EDITABLE_TYPES = setOf("image/jpeg", "image/png", "image/webp", "image/heic", "image/heif")
}
