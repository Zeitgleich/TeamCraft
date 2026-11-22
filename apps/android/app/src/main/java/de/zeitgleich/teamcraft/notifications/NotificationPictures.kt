package de.zeitgleich.teamcraft.notifications

import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.net.Uri
import android.util.Log
import de.zeitgleich.teamcraft.ui.SharedFiles
import java.io.File
import java.util.UUID
import uniffi.teamcraft_ffi.NotificationPicture

/**
 * The pictures of photo and video messages in their notifications: the sender's small preview,
 * decoded here and encoded again, so the system's notification UI, Android Auto and watches only
 * ever decode a picture this app made, never the sender's bytes. Kept in the app's cache
 * ([SharedFiles.NOTIFICATION_PICTURES_DIR]) and handed out as a content URI; the system grants its
 * notification UI and listeners read access for as long as the notification shows. A picture goes
 * when no shown line names it any more ([prune]), and all of them at sign-out ([clear]).
 */
class NotificationPictures(private val context: Context, private val nowMs: () -> Long = System::currentTimeMillis) {
  private val dir: File get() = File(context.cacheDir, SharedFiles.NOTIFICATION_PICTURES_DIR)

  /** Decodes `picture` and keeps it; `null` when it doesn't decode or is out of bounds. */
  fun keep(picture: NotificationPicture, label: String): LinePicture? {
    val bitmap = decode(picture.bytes) ?: return null
    return try {
      val hasAlpha = bitmap.hasAlpha()
      val (format, mimeType, extension) = if (hasAlpha) {
        Triple(Bitmap.CompressFormat.PNG, "image/png", "png")
      } else {
        Triple(Bitmap.CompressFormat.JPEG, "image/jpeg", "jpg")
      }
      val file = File(dir.apply { mkdirs() }, "${UUID.randomUUID()}.$extension")
      file.outputStream().use { out -> check(bitmap.compress(format, JPEG_QUALITY, out)) { "not encoded" } }
      LinePicture(fileName = file.name, mimeType = mimeType, label = label)
    } catch (failure: Exception) {
      // The notification shows the text alone.
      Log.w(TAG, "notification picture not kept: ${failure.javaClass.simpleName}")
      null
    } finally {
      bitmap.recycle()
    }
  }

  /** The content URI of a kept picture, or `null` when it is gone (the system cleared the cache). */
  fun uri(picture: LinePicture): Uri? {
    val file = File(dir, picture.fileName)
    if (file.name != picture.fileName || !file.isFile) return null
    return runCatching { SharedFiles.uriFor(context, file) }.getOrNull()
  }

  /**
   * Deletes the pictures no shown line names. One kept a moment ago may not be in a line yet
   * (another wake-up is being shown meanwhile), so only older ones go.
   */
  fun prune(shown: ShownNotifications) {
    val named = shown.conversations.values.flatMap { it.lines }.mapNotNull { it.maybePicture?.fileName }.toSet()
    val files = dir.listFiles() ?: return
    val cutoffMs = nowMs() - GRACE_MS
    files.filter { it.name !in named && it.lastModified() < cutoffMs }.forEach { it.delete() }
  }

  /** Every kept picture goes (sign-out). */
  fun clear() {
    dir.deleteRecursively()
  }

  private fun decode(bytes: ByteArray): Bitmap? {
    val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
    BitmapFactory.decodeByteArray(bytes, 0, bytes.size, bounds)
    val width = bounds.outWidth
    val height = bounds.outHeight
    if (width <= 0 || height <= 0 || width > MAX_SIDE_PX || height > MAX_SIDE_PX) return null
    var sample = 1
    while (maxOf(width, height) / (sample * 2) >= SHOWN_SIDE_PX) sample *= 2
    val options = BitmapFactory.Options().apply { inSampleSize = sample }
    return runCatching { BitmapFactory.decodeByteArray(bytes, 0, bytes.size, options) }.getOrNull()
  }

  private companion object {
    /** Senders make previews of at most 640 px; a picture larger than this is no preview. */
    const val MAX_SIDE_PX = 4096
    /** About what a notification shows of a picture at most. */
    const val SHOWN_SIDE_PX = 1024
    const val JPEG_QUALITY = 85
    const val GRACE_MS = 60_000L
    const val TAG = "TeamCraftNotify"
  }
}
