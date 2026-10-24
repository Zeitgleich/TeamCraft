package de.zeitgleich.teamcraft.ui

import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.ImageDecoder
import android.graphics.Matrix
import android.media.ExifInterface
import android.os.Build
import android.util.LruCache
import androidx.compose.runtime.Composable
import androidx.compose.runtime.State
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import java.io.File
import java.nio.ByteBuffer
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import de.zeitgleich.teamcraft.core.FileInfo

/**
 * Thumbnails are decoded at up to this size: senders make them 640 px on the longer side, older
 * clients 320 px, and a link preview's picture may be larger.
 */
const val THUMBNAIL_DECODE_PX = 1024

/** Share of the app's heap that decoded thumbnails may take (the usual eighth). */
private const val MEMORY_SHARE = 8L

/**
 * Full images kept, by count: the one being looked at and its neighbours, so swiping back and
 * forth doesn't decode again. Counted, not sized: one full image may be larger than the
 * thumbnails' whole budget, which would evict it at once.
 */
private const val FULL_IMAGES_KEPT = 4

/**
 * Decoded pictures – the senders' thumbnails and full images from the decrypted downloads – kept in
 * memory so scrolling back or reopening an image doesn't decode again (#315). Bounded in bytes, the
 * least recently used goes first. Decoding runs off the main thread and samples down to the size
 * the picture is shown at. The decrypted files themselves are the disk cache (the core's download
 * folder in app storage, which it keeps under a size limit).
 */
object Pictures {
  private val memory = object : LruCache<String, ImageBitmap>(
    (Runtime.getRuntime().maxMemory() / MEMORY_SHARE).coerceAtMost(Int.MAX_VALUE.toLong()).toInt(),
  ) {
    override fun sizeOf(key: String, value: ImageBitmap): Int = value.width * value.height * BYTES_PER_PIXEL
  }

  private val fullImages = LruCache<String, ImageBitmap>(FULL_IMAGES_KEPT)

  /** A picture decoded before, without waiting. */
  fun cached(key: String): ImageBitmap? = memory.get(key) ?: fullImages.get(key)

  /** Forgets every decoded picture: the account they were decrypted for is gone from this device's session. */
  fun clear() {
    memory.evictAll()
    fullImages.evictAll()
  }

  /** A thumbnail's bytes, decoded once per file. */
  suspend fun thumbnail(key: String, bytes: ByteArray, maxSidePx: Int): ImageBitmap? =
    cached(key) ?: withContext(Dispatchers.Default) {
      decodeBytes(bytes, maxSidePx)?.asImageBitmap()?.also { memory.put(key, it) }
    }

  /** A decrypted image file at about `maxSidePx` on its longer side, upright. */
  suspend fun file(path: String, maxSidePx: Int): ImageBitmap? {
    val key = fileKey(path, maxSidePx)
    return cached(key) ?: withContext(Dispatchers.IO) {
      // The core keeps the most recently used downloads when it trims its folder.
      File(path).setLastModified(System.currentTimeMillis())
      decodeFile(path, maxSidePx)?.asImageBitmap()?.also { fullImages.put(key, it) }
    }
  }

  fun fileKey(path: String, maxSidePx: Int): String = "file:$maxSidePx:$path"

  /**
   * Width over height of a picture's bytes, read from its header only (no pixels decoded), so a
   * preview gets its final size before the picture is there; `null` for unreadable bytes.
   */
  fun aspectRatio(bytes: ByteArray): Float? {
    val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
    BitmapFactory.decodeByteArray(bytes, 0, bytes.size, bounds)
    return if (bounds.outWidth > 0 && bounds.outHeight > 0) bounds.outWidth.toFloat() / bounds.outHeight else null
  }

  private fun decodeBytes(bytes: ByteArray, maxSidePx: Int): Bitmap? =
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
      decodeSource(ImageDecoder.createSource(ByteBuffer.wrap(bytes)), maxSidePx)
    } else {
      val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
      BitmapFactory.decodeByteArray(bytes, 0, bytes.size, bounds)
      val options = BitmapFactory.Options().apply { inSampleSize = sampleSize(bounds.outWidth, bounds.outHeight, maxSidePx) }
      BitmapFactory.decodeByteArray(bytes, 0, bytes.size, options)
    }

  private fun decodeFile(path: String, maxSidePx: Int): Bitmap? =
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
      // ImageDecoder applies the EXIF orientation itself.
      decodeSource(ImageDecoder.createSource(File(path)), maxSidePx)
    } else {
      val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
      BitmapFactory.decodeFile(path, bounds)
      val options = BitmapFactory.Options().apply { inSampleSize = sampleSize(bounds.outWidth, bounds.outHeight, maxSidePx) }
      BitmapFactory.decodeFile(path, options)?.let { uprighted(it, path) }
    }

  private fun decodeSource(source: ImageDecoder.Source, maxSidePx: Int): Bitmap? =
    runCatching {
      ImageDecoder.decodeBitmap(source) { decoder, info, _ ->
        decoder.setTargetSampleSize(sampleSize(info.size.width, info.size.height, maxSidePx))
      }
    }.getOrNull()
}

/** Decoded pictures cost about four bytes a pixel (ARGB_8888, in the heap or on the GPU). */
private const val BYTES_PER_PIXEL = 4

/** The largest power of two that keeps the longer side at `maxSidePx` or more. */
internal fun sampleSize(width: Int, height: Int, maxSidePx: Int): Int {
  var sample = 1
  while (maxOf(width, height) / (sample * 2) >= maxSidePx) sample *= 2
  return sample
}

/** Camera photos are often stored sideways with an EXIF orientation; turn them upright. */
private fun uprighted(bitmap: Bitmap, path: String): Bitmap {
  val orientation = runCatching {
    ExifInterface(path).getAttributeInt(ExifInterface.TAG_ORIENTATION, ExifInterface.ORIENTATION_NORMAL)
  }.getOrDefault(ExifInterface.ORIENTATION_NORMAL)
  val degrees = when (orientation) {
    ExifInterface.ORIENTATION_ROTATE_90 -> 90f
    ExifInterface.ORIENTATION_ROTATE_180 -> 180f
    ExifInterface.ORIENTATION_ROTATE_270 -> 270f
    else -> return bitmap
  }
  val matrix = Matrix().apply { postRotate(degrees) }
  return Bitmap.createBitmap(bitmap, 0, 0, bitmap.width, bitmap.height, matrix, true)
}

/** The memory cache's key of a file's thumbnail. */
fun thumbnailKey(file: FileInfo): String = "thumb:${file.message}:${file.fileId}"

/**
 * A thumbnail for the composition: at once when it was decoded before, otherwise decoded in the
 * background (the slot shows a placeholder meanwhile).
 */
@Composable
fun rememberThumbnail(key: String, maybeBytes: ByteArray?, maxSidePx: Int): State<ImageBitmap?> {
  val initial = remember(key) { Pictures.cached(key) }
  return produceState(initial, key, maybeBytes) {
    if (value == null && maybeBytes != null) value = Pictures.thumbnail(key, maybeBytes, maxSidePx)
  }
}
