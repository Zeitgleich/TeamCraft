package de.zeitgleich.teamcraft.notifications

import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.BitmapShader
import android.graphics.Canvas
import android.graphics.Paint
import android.graphics.Shader
import android.util.Log
import androidx.core.graphics.drawable.IconCompat
import java.io.File
import java.util.concurrent.ConcurrentHashMap
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import uniffi.teamcraft_ffi.TeamCraft

/**
 * The profile pictures of the people who write to the user, for notifications. A picture is read
 * from the core on demand (never from the push payload), kept as a file in the app's cache folder
 * and drawn as a circle. Without a picture, or until it arrives, the caller falls back to the
 * generated initials ([PersonIcons]). Pictures are refreshed after [PICTURE_MAX_AGE_MS]; people
 * without one are asked again after [MISSING_MAX_AGE_MS].
 */
class AvatarCache(
  private val context: Context,
  private val core: suspend () -> TeamCraft,
) {
  private val inFlight = ConcurrentHashMap.newKeySet<String>()

  /** The circular picture of `userId` if one is cached; never touches the network. */
  fun maybeIcon(userId: String): IconCompat? = maybeBitmap(userId)?.let(IconCompat::createWithBitmap)

  fun maybeBitmap(userId: String): Bitmap? {
    val file = pictureFile(userId) ?: return null
    if (!file.isFile) return null
    return decodeCircle(file)
  }

  /**
   * Makes sure a fresh picture is cached. Answers `true` when a new picture arrived, so the caller
   * can update what it already shows; `false` for no change, no picture, or a failure (a failed
   * fetch is retried with the next message).
   */
  suspend fun refresh(userId: String): Boolean {
    val picture = pictureFile(userId) ?: return false
    val missing = missingFile(userId) ?: return false
    if (isFresh(picture, PICTURE_MAX_AGE_MS) || isFresh(missing, MISSING_MAX_AGE_MS)) return false
    if (!inFlight.add(userId)) return false
    return try {
      withContext(Dispatchers.IO) {
        val bytes = core().avatarPicture(userId, MAX_PICTURE_BYTES)
        if (bytes == null) {
          picture.delete()
          missing.parentFile?.mkdirs()
          missing.writeBytes(ByteArray(0))
          missing.setLastModified(System.currentTimeMillis())
          false
        } else if (!isDecodable(bytes)) {
          false
        } else {
          picture.parentFile?.mkdirs()
          picture.writeBytes(bytes)
          missing.delete()
          true
        }
      }
    } catch (cancelled: CancellationException) {
      throw cancelled
    } catch (failure: Exception) {
      // Offline, signed out, or a storage error: the initials stay, the next message tries again.
      Log.w(TAG, "avatar not fetched: ${failure.javaClass.simpleName}")
      false
    } finally {
      inFlight.remove(userId)
    }
  }

  /** Forgets every picture (sign-out): they belong to the account that is gone. */
  fun clear() {
    runCatching { directory().deleteRecursively() }
  }

  private fun directory() = File(context.cacheDir, DIRECTORY)

  /** Only numeric user IDs name a file; anything else (a display name used as key) has no picture. */
  private fun pictureFile(userId: String): File? =
    userId.takeIf { it.isNotEmpty() && it.all(Char::isDigit) }?.let { File(directory(), "$it.img") }

  private fun missingFile(userId: String): File? = pictureFile(userId)?.let { File(directory(), "${it.nameWithoutExtension}.none") }

  private fun isFresh(file: File, maxAgeMs: Long): Boolean =
    file.isFile && System.currentTimeMillis() - file.lastModified() < maxAgeMs

  private fun isDecodable(bytes: ByteArray): Boolean {
    val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
    BitmapFactory.decodeByteArray(bytes, 0, bytes.size, bounds)
    return bounds.outWidth > 0 && bounds.outHeight > 0
  }

  private fun decodeCircle(file: File): Bitmap? {
    val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
    BitmapFactory.decodeFile(file.path, bounds)
    if (bounds.outWidth <= 0 || bounds.outHeight <= 0) return null
    val options = BitmapFactory.Options().apply {
      inSampleSize = sampleSize(minOf(bounds.outWidth, bounds.outHeight))
    }
    val source = BitmapFactory.decodeFile(file.path, options) ?: return null
    return circle(source)
  }

  companion object {
    private const val DIRECTORY = "avatars"
    private const val TAG = "TeamCraftNotify"
    private const val SIZE_PX = 128
    private const val MAX_PICTURE_BYTES = 2u * 1024u * 1024u
    private const val PICTURE_MAX_AGE_MS = 12L * 60 * 60 * 1000
    private const val MISSING_MAX_AGE_MS = 60L * 60 * 1000

    /** The largest power of two that keeps the shorter side at least [SIZE_PX]. */
    fun sampleSize(shorterSidePx: Int): Int {
      var sample = 1
      while (shorterSidePx / (sample * 2) >= SIZE_PX) sample *= 2
      return sample
    }

    /** `source` centre-cropped to a square and drawn as a circle of [SIZE_PX]. */
    fun circle(source: Bitmap): Bitmap {
      val side = minOf(source.width, source.height)
      val square = Bitmap.createBitmap(source, (source.width - side) / 2, (source.height - side) / 2, side, side)
      val scaled = Bitmap.createScaledBitmap(square, SIZE_PX, SIZE_PX, true)
      val result = Bitmap.createBitmap(SIZE_PX, SIZE_PX, Bitmap.Config.ARGB_8888)
      val paint = Paint(Paint.ANTI_ALIAS_FLAG).apply { shader = BitmapShader(scaled, Shader.TileMode.CLAMP, Shader.TileMode.CLAMP) }
      Canvas(result).drawCircle(SIZE_PX / 2f, SIZE_PX / 2f, SIZE_PX / 2f, paint)
      return result
    }
  }
}
