package de.zeitgleich.teamcraft.ui.media

import android.content.Context
import android.graphics.Matrix
import android.graphics.RectF
import android.net.Uri
import android.util.LruCache
import androidx.compose.foundation.Canvas
import androidx.compose.runtime.Composable
import androidx.compose.runtime.State
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.drawscope.drawIntoCanvas
import androidx.compose.ui.graphics.nativeCanvas
import androidx.compose.ui.platform.LocalContext
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/** Where a picture sits in a view: source pixels to view pixels, and the rectangle it fills. */
class PictureFit(val toView: Matrix, val bounds: RectF) {
  /** View pixels per unit of the picture (marks are measured in units). */
  fun pixelsPerUnit(picture: EditablePicture): Float {
    val values = FloatArray(9).also(toView::getValues)
    val scale = kotlin.math.sqrt(kotlin.math.abs(values[Matrix.MSCALE_X] * values[Matrix.MSCALE_Y] - values[Matrix.MSKEW_X] * values[Matrix.MSKEW_Y]))
    return scale * picture.unit
  }

  /** A point of the view in the picture's units, `null` when the view can't be undone. */
  fun toUnits(picture: EditablePicture, x: Float, y: Float): Pair<Float, Float>? {
    val inverse = Matrix()
    if (!toView.invert(inverse)) return null
    val point = floatArrayOf(x, y)
    inverse.mapPoints(point)
    return point[0] / picture.unit to point[1] / picture.unit
  }

  companion object {
    /**
     * Fits the edited picture into a `width` × `height` view with `margin` around it: its crop
     * (`isFramed`) or the whole turned picture (the crop screen draws the frame over that).
     */
    fun of(picture: EditablePicture, crop: CropEdit, isFramed: Boolean, width: Float, height: Float, margin: Float): PictureFit {
      val (contentWidth, contentHeight) = if (isFramed) {
        PictureGeometry.outputSize(picture.width, picture.height, crop)
      } else {
        PictureGeometry.turnedSize(picture.width, picture.height, crop.quarterTurnsLeft)
      }
      val scale = minOf((width - margin * 2) / contentWidth, (height - margin * 2) / contentHeight)
      val left = (width - contentWidth * scale) / 2
      val top = (height - contentHeight * scale) / 2
      val toView = PictureRenderer.transform(picture.width, picture.height, crop, isFramed).apply {
        postScale(scale, scale)
        postTranslate(left, top)
      }
      return PictureFit(toView, RectF(left, top, left + contentWidth * scale, top + contentHeight * scale))
    }
  }
}

/** Draws a picture with its edits, clipped to where it sits. */
@Composable
fun EditedPictureCanvas(picture: EditablePicture, edit: PictureEdit, fit: PictureFit, modifier: Modifier = Modifier) {
  val context = LocalContext.current
  Canvas(modifier) {
    drawIntoCanvas { canvas ->
      val native = canvas.nativeCanvas
      native.save()
      native.clipRect(fit.bounds)
      PictureRenderer.draw(context, native, picture, edit, fit.toView)
      native.restore()
    }
  }
}

/**
 * A picture of the flow decoded for editing, kept while it is likely needed again (the review's
 * pager and the editors open the same ones). Bounded in bytes.
 */
@Composable
fun rememberEditablePicture(uri: String): State<EditablePicture?> {
  val context = LocalContext.current
  return produceState(EditablePictures.cached(uri), uri) {
    if (value == null) value = EditablePictures.load(context, uri)
  }
}

object EditablePictures {
  private val cache = object : LruCache<String, EditablePicture>((Runtime.getRuntime().maxMemory() / 6).toInt()) {
    override fun sizeOf(key: String, value: EditablePicture): Int = value.bitmap.byteCount
  }

  fun cached(uri: String): EditablePicture? = cache.get(uri)

  suspend fun load(context: Context, uri: String): EditablePicture? = cached(uri) ?: withContext(Dispatchers.IO) {
    PictureRenderer.decodeUpright(context.contentResolver, Uri.parse(uri), EDIT_PREVIEW_SIDE_PX)
      ?.let(EditablePicture::of)
      ?.also { cache.put(uri, it) }
  }

  fun clear() = cache.evictAll()
}

/**
 * Undo and redo for an editor: [record] keeps the state before a change, so one gesture is one
 * step however many frames it took.
 */
class EditHistory<T>(initial: T) {
  var value by mutableStateOf(initial)
  private var past by mutableStateOf(listOf<T>())
  private var future by mutableStateOf(listOf<T>())

  val canUndo: Boolean get() = past.isNotEmpty()
  val canRedo: Boolean get() = future.isNotEmpty()

  /** Call before a change: the current state becomes an undo step. */
  fun record() {
    past = (past + value).takeLast(MAX_STEPS)
    future = emptyList()
  }

  /** One change as one step. */
  fun set(new: T) {
    if (new == value) return
    record()
    value = new
  }

  fun undo() {
    val previous = past.lastOrNull() ?: return
    future = listOf(value) + future
    past = past.dropLast(1)
    value = previous
  }

  fun redo() {
    val next = future.firstOrNull() ?: return
    past = past + value
    future = future.drop(1)
    value = next
  }

  private companion object {
    /** Steps kept: plenty for a picture, bounded for a long drawing session. */
    const val MAX_STEPS = 100
  }
}

@Composable
fun <T> rememberEditHistory(key: Any, initial: T): EditHistory<T> = remember(key) { EditHistory(initial) }
