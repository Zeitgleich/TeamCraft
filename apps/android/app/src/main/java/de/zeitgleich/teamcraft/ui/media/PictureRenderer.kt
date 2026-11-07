package de.zeitgleich.teamcraft.ui.media

import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.BitmapShader
import android.graphics.Canvas
import android.graphics.ImageDecoder
import android.graphics.Matrix
import android.graphics.Paint
import android.graphics.Path
import android.graphics.PointF
import android.graphics.Shader
import android.graphics.Typeface
import android.media.ExifInterface
import android.media.FaceDetector
import android.net.Uri
import android.os.Build
import androidx.annotation.DrawableRes
import androidx.core.content.ContextCompat
import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream
import java.io.InputStream
import java.nio.ByteBuffer
import kotlin.math.roundToInt
import kotlin.math.sqrt
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.ui.sampleSize

/** Longest side a picture is shown at while editing: sharp on a phone, quick to draw over. */
const val EDIT_PREVIEW_SIDE_PX = 1600

/** The blur's copy is this many times smaller than the picture: faces turn into soft shapes. */
private const val BLUR_DIVISOR = 24

/** Faces the on-device detector looks for at most, and the size it looks at (it is slow when larger). */
private const val MAX_FACES = 16
private const val FACE_SEARCH_SIDE_PX = 960

/** A face's blur reaches this many eye distances from its centre: forehead to chin. */
private const val FACE_RADIUS_EYES = 1.7f

/** The highlighter lets the picture show through. */
private const val HIGHLIGHTER_ALPHA = 0x70

/** Text and stickers are outlined with this share of their size, for contrast on any background. */
private const val OUTLINE_SHARE = 0.08f

/** A picture decoded for editing, with the blurred copy the blur brush paints with. */
class EditablePicture(val bitmap: Bitmap, private val blurred: Bitmap) {
  val width: Float get() = bitmap.width.toFloat()
  val height: Float get() = bitmap.height.toFloat()

  /** The picture's unit: its longer side in pixels (marks are stored in these units). */
  val unit: Float get() = maxOf(width, height)

  fun blurPaint(): Paint = Paint(Paint.ANTI_ALIAS_FLAG or Paint.FILTER_BITMAP_FLAG).apply {
    shader = BitmapShader(blurred, Shader.TileMode.CLAMP, Shader.TileMode.CLAMP).apply {
      setLocalMatrix(Matrix().apply { setScale(width / blurred.width, height / blurred.height) })
    }
  }

  companion object {
    fun of(bitmap: Bitmap): EditablePicture = EditablePicture(bitmap, blurredCopy(bitmap))

    private fun blurredCopy(bitmap: Bitmap): Bitmap {
      // Halving step by step averages the pixels; one big step would just pick some of them.
      var copy = bitmap
      val target = maxOf(8, maxOf(bitmap.width, bitmap.height) / BLUR_DIVISOR)
      while (maxOf(copy.width, copy.height) / 2 >= target) {
        val next = Bitmap.createScaledBitmap(copy, maxOf(1, copy.width / 2), maxOf(1, copy.height / 2), true)
        if (copy !== bitmap) copy.recycle()
        copy = next
      }
      return if (copy === bitmap) bitmap.copy(Bitmap.Config.ARGB_8888, false) else copy
    }
  }
}

/**
 * Draws a picture with its edits, the same way on the screen and into the file that is sent, so
 * what the user sees is what goes out. Everything happens on the device.
 */
object PictureRenderer {
  /**
   * Source pixels to the crop's pixels: quarter turns, mirror, straightening (zoomed in to cover),
   * then the frame (`isFramed`) or the whole turned picture (the crop screen shows that).
   */
  fun transform(width: Float, height: Float, crop: CropEdit, isFramed: Boolean = true): Matrix {
    val (turnedWidth, turnedHeight) = PictureGeometry.turnedSize(width, height, crop.quarterTurnsLeft)
    val scale = PictureGeometry.coverScale(turnedWidth, turnedHeight, crop.straightenDegrees)
    return Matrix().apply {
      postTranslate(-width / 2, -height / 2)
      postRotate(-QUARTER * crop.quarterTurnsLeft.mod(4))
      if (crop.isMirrored) postScale(-1f, 1f)
      postRotate(crop.straightenDegrees)
      postScale(scale, scale)
      postTranslate(turnedWidth / 2, turnedHeight / 2)
      if (isFramed) postTranslate(-crop.frame.left * turnedWidth, -crop.frame.top * turnedHeight)
    }
  }

  /** Draws the picture and its marks through `toCanvas` (source pixels to canvas pixels). */
  fun draw(context: Context, canvas: Canvas, picture: EditablePicture, edit: PictureEdit, toCanvas: Matrix) {
    canvas.save()
    canvas.concat(toCanvas)
    canvas.drawBitmap(picture.bitmap, 0f, 0f, Paint(Paint.FILTER_BITMAP_FLAG))
    drawBlur(canvas, picture, edit)
    for (mark in edit.marks) {
      when (mark) {
        is StrokeMark -> if (mark.tool != StrokeTool.Blur) drawStroke(canvas, picture.unit, mark)
        is TextMark -> drawText(canvas, picture.unit, mark)
        is StickerMark -> drawSticker(context, canvas, picture.unit, mark)
      }
    }
    canvas.restore()
  }

  /**
   * The edited picture for sending, at the quality's size: a new JPEG (or PNG for a PNG), so no
   * metadata of the original (place, camera) goes along.
   */
  suspend fun render(context: Context, uri: Uri, edit: PictureEdit, quality: MediaQuality): RenderedPicture? =
    withContext(Dispatchers.Default) {
      val maxSide = maxSidePx(quality)
      val source = decodeUpright(context, uri, maxSide) ?: return@withContext null
      val picture = EditablePicture.of(source)
      val (outWidth, outHeight) = PictureGeometry.outputSize(picture.width, picture.height, edit.crop)
      val scale = PictureGeometry.downscale(outWidth, outHeight, maxSide)
      val width = (outWidth * scale).roundToInt().coerceAtLeast(1)
      val height = (outHeight * scale).roundToInt().coerceAtLeast(1)
      val isPng = context.contentResolver.getType(uri) == "image/png"
      val output = Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888)
      if (!isPng) output.eraseColor(android.graphics.Color.BLACK)
      val toOutput = transform(picture.width, picture.height, edit.crop).apply { postScale(scale, scale) }
      draw(context, Canvas(output), picture, edit, toOutput)
      source.recycle()
      val bytes = ByteArrayOutputStream().use { out ->
        output.compress(if (isPng) Bitmap.CompressFormat.PNG else Bitmap.CompressFormat.JPEG, jpegQuality(quality), out)
        out.toByteArray()
      }
      output.recycle()
      RenderedPicture(bytes, if (isPng) "image/png" else "image/jpeg")
    }

  /**
   * Faces in the picture, found on the device with the platform's detector (nothing leaves the
   * phone), as centre and radius triples in units.
   */
  suspend fun findFaces(picture: EditablePicture): List<Float> = withContext(Dispatchers.Default) {
    val scale = minOf(1f, FACE_SEARCH_SIDE_PX / picture.unit)
    // The detector wants RGB 565 and an even width.
    val width = ((picture.width * scale).roundToInt() and 1.inv()).coerceAtLeast(2)
    val height = (picture.height * scale).roundToInt().coerceAtLeast(1)
    val search = Bitmap.createBitmap(width, height, Bitmap.Config.RGB_565)
    Canvas(search).drawBitmap(picture.bitmap, Matrix().apply { setScale(width / picture.width, height / picture.height) }, Paint(Paint.FILTER_BITMAP_FLAG))
    val found = arrayOfNulls<FaceDetector.Face>(MAX_FACES)
    val count = runCatching { FaceDetector(width, height, MAX_FACES).findFaces(search, found) }.getOrDefault(0)
    search.recycle()
    val unit = maxOf(width, height).toFloat()
    found.take(count).filterNotNull().flatMap { face ->
      val middle = PointF().also(face::getMidPoint)
      val eyes = face.eyesDistance()
      // The point between the eyes is above the face's centre.
      listOf(middle.x / unit, (middle.y + eyes * 0.4f) / unit, eyes * FACE_RADIUS_EYES / unit)
    }
  }

  /**
   * A picture from the gallery or the camera, upright, at about `maxSidePx`, in software memory to
   * draw on. The camera's sealed photos ([SealedCaptures]) are opened in memory.
   */
  fun decodeUpright(context: Context, uri: Uri, maxSidePx: Int): Bitmap? = runCatching {
    val resolver = context.contentResolver
    val maybeSealed = if (SealedCaptures.isSealed(uri)) SealedCaptures.readAll(context, uri) else null
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
      // ImageDecoder applies the EXIF orientation itself.
      val source = maybeSealed?.let { ImageDecoder.createSource(ByteBuffer.wrap(it)) } ?: ImageDecoder.createSource(resolver, uri)
      ImageDecoder.decodeBitmap(source) { decoder, info, _ ->
        decoder.allocator = ImageDecoder.ALLOCATOR_SOFTWARE
        decoder.setTargetSampleSize(sampleSize(info.size.width, info.size.height, maxSidePx))
      }
    } else {
      fun open(): InputStream? = maybeSealed?.let(::ByteArrayInputStream) ?: resolver.openInputStream(uri)
      val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
      open()?.use { BitmapFactory.decodeStream(it, null, bounds) }
      val options = BitmapFactory.Options().apply { inSampleSize = sampleSize(bounds.outWidth, bounds.outHeight, maxSidePx) }
      val decoded = open()?.use { BitmapFactory.decodeStream(it, null, options) } ?: return null
      val orientation = open()?.use {
        ExifInterface(it).getAttributeInt(ExifInterface.TAG_ORIENTATION, ExifInterface.ORIENTATION_NORMAL)
      } ?: ExifInterface.ORIENTATION_NORMAL
      // Turned clockwise, then mirrored (the camera's front photos are, CapturedJpeg).
      val (degrees, isMirrored) = when (orientation) {
        ExifInterface.ORIENTATION_FLIP_HORIZONTAL -> 0f to true
        ExifInterface.ORIENTATION_ROTATE_180 -> 180f to false
        ExifInterface.ORIENTATION_FLIP_VERTICAL -> 180f to true
        ExifInterface.ORIENTATION_TRANSPOSE -> 90f to true
        ExifInterface.ORIENTATION_ROTATE_90 -> 90f to false
        ExifInterface.ORIENTATION_TRANSVERSE -> 270f to true
        ExifInterface.ORIENTATION_ROTATE_270 -> 270f to false
        else -> return decoded
      }
      val matrix = Matrix().apply {
        postRotate(degrees)
        if (isMirrored) postScale(-1f, 1f)
      }
      Bitmap.createBitmap(decoded, 0, 0, decoded.width, decoded.height, matrix, true)
    }
  }.getOrNull()

  /**
   * The 2 × 2 matrix that undoes the turn and mirror of `toView` (normalized, no scale): a text or
   * sticker placed with it reads upright in that view.
   */
  fun upright(toView: Matrix): List<Float> {
    val values = FloatArray(9).also(toView::getValues)
    val a = values[Matrix.MSCALE_X]
    val b = values[Matrix.MSKEW_X]
    val c = values[Matrix.MSKEW_Y]
    val d = values[Matrix.MSCALE_Y]
    val determinant = a * d - b * c
    if (determinant == 0f) return listOf(1f, 0f, 0f, 1f)
    val scale = sqrt(kotlin.math.abs(determinant))
    // The inverse of the view's linear part, divided by its scale.
    return listOf(d / determinant * scale, -b / determinant * scale, -c / determinant * scale, a / determinant * scale)
  }

  /** Half the width and height of a text or sticker in units, in its own upright frame. */
  fun halfExtent(mark: Mark): Pair<Float, Float> = when (mark) {
    is TextMark -> {
      val paint = textPaint(mark.size * TEXT_MEASURE_UNIT)
      val lines = mark.text.lines()
      val widest = lines.maxOf { paint.measureText(it) } / TEXT_MEASURE_UNIT
      (widest / 2 + mark.size * 0.2f) to (mark.size * LINE_HEIGHT * lines.size / 2)
    }
    is StickerMark -> mark.size / 2 to mark.size / 2
    is StrokeMark -> 0f to 0f
  }

  @DrawableRes
  fun stickerIcon(sticker: Sticker): Int = when (sticker) {
    Sticker.Arrow -> R.drawable.ic_arrow_right_alt
    Sticker.Circle -> R.drawable.ic_circle
    Sticker.Square -> R.drawable.ic_crop_square
    Sticker.Check -> R.drawable.ic_check_circle
    Sticker.Star -> R.drawable.ic_star
    Sticker.Heart -> R.drawable.ic_favorite
    Sticker.Question -> R.drawable.ic_help
  }

  fun maxSidePx(quality: MediaQuality): Int = when (quality) {
    MediaQuality.Standard -> STANDARD_SIDE_PX
    MediaQuality.High -> HIGH_SIDE_PX
  }

  private fun jpegQuality(quality: MediaQuality): Int = when (quality) {
    MediaQuality.Standard -> 80
    MediaQuality.High -> 92
  }

  private fun drawBlur(canvas: Canvas, picture: EditablePicture, edit: PictureEdit) {
    val strokes = edit.marks.filterIsInstance<StrokeMark>().filter { it.tool == StrokeTool.Blur }
    val faces = edit.maybeFaces.orEmpty().takeIf { edit.isBlurringFaces }.orEmpty()
    if (strokes.isEmpty() && faces.isEmpty()) return
    val unit = picture.unit
    val paint = picture.blurPaint()
    for (i in 0 until faces.size / 3) {
      canvas.drawCircle(faces[i * 3] * unit, faces[i * 3 + 1] * unit, faces[i * 3 + 2] * unit, paint)
    }
    paint.style = Paint.Style.STROKE
    paint.strokeCap = Paint.Cap.ROUND
    paint.strokeJoin = Paint.Join.ROUND
    for (stroke in strokes) {
      paint.strokeWidth = stroke.width * unit
      canvas.drawPath(strokePath(stroke.points, unit), paint)
    }
  }

  private fun drawStroke(canvas: Canvas, unit: Float, mark: StrokeMark) {
    val paint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
      style = Paint.Style.STROKE
      strokeWidth = mark.width * unit
      strokeJoin = Paint.Join.ROUND
      if (mark.tool == StrokeTool.Highlighter) {
        strokeCap = Paint.Cap.SQUARE
        color = (mark.color and 0x00FFFFFF) or (HIGHLIGHTER_ALPHA shl 24)
      } else {
        strokeCap = Paint.Cap.ROUND
        color = mark.color
      }
    }
    if (mark.points.size == 2) {
      // A tap is a dot.
      paint.style = Paint.Style.FILL
      canvas.drawCircle(mark.points[0] * unit, mark.points[1] * unit, mark.width * unit / 2, paint)
      return
    }
    canvas.drawPath(strokePath(mark.points, unit), paint)
  }

  /** Smooth through the points: curves to the midpoints, as finger strokes are drawn in most apps. */
  private fun strokePath(points: List<Float>, unit: Float): Path = Path().apply {
    if (points.size < 2) return@apply
    moveTo(points[0] * unit, points[1] * unit)
    var i = 2
    while (i + 1 < points.size) {
      val x = points[i] * unit
      val y = points[i + 1] * unit
      if (i + 3 < points.size) {
        quadTo(x, y, (x + points[i + 2] * unit) / 2, (y + points[i + 3] * unit) / 2)
      } else {
        lineTo(x, y)
      }
      i += 2
    }
  }

  private fun drawText(canvas: Canvas, unit: Float, mark: TextMark) {
    val size = mark.size * unit
    val fill = textPaint(size).apply { color = mark.color }
    val outline = textPaint(size).apply {
      color = contrastTo(mark.color)
      style = Paint.Style.STROKE
      strokeWidth = size * OUTLINE_SHARE
      strokeJoin = Paint.Join.ROUND
    }
    canvas.save()
    canvas.translate(mark.x * unit, mark.y * unit)
    canvas.concat(uprightMatrix(mark.upright))
    val lines = mark.text.lines()
    val lineHeight = size * LINE_HEIGHT
    val firstBaseline = -lineHeight * lines.size / 2 - fill.fontMetrics.ascent + (lineHeight - size) / 2
    lines.forEachIndexed { index, line ->
      val y = firstBaseline + index * lineHeight
      canvas.drawText(line, 0f, y, outline)
      canvas.drawText(line, 0f, y, fill)
    }
    canvas.restore()
  }

  private fun drawSticker(context: Context, canvas: Canvas, unit: Float, mark: StickerMark) {
    val drawable = ContextCompat.getDrawable(context, stickerIcon(mark.sticker))?.mutate() ?: return
    val half = (mark.size * unit / 2).roundToInt()
    canvas.save()
    canvas.translate(mark.x * unit, mark.y * unit)
    canvas.concat(uprightMatrix(mark.upright))
    // A slightly larger copy in the contrast colour behind it, as an outline.
    val outline = (half * (1 + OUTLINE_SHARE)).roundToInt()
    drawable.setTint(contrastTo(mark.color))
    drawable.setBounds(-outline, -outline, outline, outline)
    drawable.draw(canvas)
    drawable.setTint(mark.color)
    drawable.setBounds(-half, -half, half, half)
    drawable.draw(canvas)
    canvas.restore()
  }

  private fun textPaint(size: Float) = Paint(Paint.ANTI_ALIAS_FLAG or Paint.SUBPIXEL_TEXT_FLAG).apply {
    textSize = size
    typeface = Typeface.DEFAULT_BOLD
    textAlign = Paint.Align.CENTER
  }

  private fun uprightMatrix(values: List<Float>): Matrix = Matrix().apply {
    if (values.size == 4) setValues(floatArrayOf(values[0], values[1], 0f, values[2], values[3], 0f, 0f, 0f, 1f))
  }

  /** Black around light colours, white around dark ones. */
  private fun contrastTo(color: Int): Int {
    val r = (color shr 16) and 0xFF
    val g = (color shr 8) and 0xFF
    val b = color and 0xFF
    val luminance = 0.299f * r + 0.587f * g + 0.114f * b
    return if (luminance > 150f) 0xCC000000.toInt() else 0xCCFFFFFF.toInt()
  }

  private const val QUARTER = 90f
  private const val LINE_HEIGHT = 1.2f

  /** Text is measured at this many pixels per unit; any size works, the result is in units. */
  private const val TEXT_MEASURE_UNIT = 1000f

  /** Standard: sharp on any phone and a few hundred kilobytes. High: close to the camera's own. */
  private const val STANDARD_SIDE_PX = 1600
  private const val HIGH_SIDE_PX = 4096
}

class RenderedPicture(val bytes: ByteArray, val mediaType: String)
