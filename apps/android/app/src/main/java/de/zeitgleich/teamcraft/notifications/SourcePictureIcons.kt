package de.zeitgleich.teamcraft.notifications

import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.util.Log
import android.util.LruCache
import androidx.core.graphics.drawable.IconCompat
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import uniffi.teamcraft_ffi.TeamCraft

/**
 * The pictures of integration sources (docs/bridge.md, "Source identity") for notifications
 * (#1956): a bot's message shows the picture set for its source in the channel, as the message feed
 * does. Read from this device's encrypted store, never from the network or the push; the last
 * [MEMORY_ENTRIES] decoded circles stay in memory. Until one is read, or without one, the bot's
 * avatar or the initials show.
 */
class SourcePictureIcons(private val core: suspend () -> TeamCraft) {
  private val memory = LruCache<String, Bitmap>(MEMORY_ENTRIES)

  /** The circle of the picture if it is decoded already; never touches the store. */
  fun maybeIcon(conversationId: String, sha256: String): IconCompat? =
    memory.get(key(conversationId, sha256))?.let(IconCompat::createWithBitmap)

  /** Reads and decodes the picture; `true` when it is newly in memory, so the caller can post again. */
  suspend fun load(conversationId: String, sha256: String): Boolean = withContext(Dispatchers.IO) {
    val key = key(conversationId, sha256)
    if (memory.get(key) != null) return@withContext false
    try {
      val bytes = core().integrationSourcePicture(conversationId, sha256) ?: return@withContext false
      val circle = decodeCircle(bytes) ?: return@withContext false
      memory.put(key, circle)
      true
    } catch (cancelled: CancellationException) {
      throw cancelled
    } catch (failure: Exception) {
      // Locked store or signed out: the stand-in stays, as for a source without a picture.
      Log.w(TAG, "source picture not read: ${failure.javaClass.simpleName}")
      false
    }
  }

  /** Forgets every picture (sign-out): they belong to the account that is gone. */
  fun clear() {
    memory.evictAll()
  }

  private fun decodeCircle(bytes: ByteArray): Bitmap? {
    val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
    BitmapFactory.decodeByteArray(bytes, 0, bytes.size, bounds)
    if (!AvatarCache.isReasonable(bounds.outWidth, bounds.outHeight)) return null
    val options = BitmapFactory.Options().apply {
      inSampleSize = AvatarCache.sampleSize(minOf(bounds.outWidth, bounds.outHeight))
    }
    val source = BitmapFactory.decodeByteArray(bytes, 0, bytes.size, options) ?: return null
    return AvatarCache.circle(source)
  }

  private fun key(conversationId: String, sha256: String) = "$conversationId/$sha256"

  companion object {
    private const val TAG = "TeamCraftNotify"

    /** A notification shows a few sources at most; 128 px circles are 64 KiB each. */
    private const val MEMORY_ENTRIES = 32
  }
}
