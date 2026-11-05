package de.zeitgleich.teamcraft.ui

import android.Manifest
import android.content.ContentUris
import android.content.Context
import android.content.pm.PackageManager
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.net.Uri
import android.os.Build
import android.provider.MediaStore
import android.util.Size
import androidx.core.content.ContextCompat
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/** How many of the newest photos and videos the attach sheet shows. */
const val RECENT_MEDIA_LIMIT = 40

/** What the app may read of the device's photos and videos. */
enum class MediaAccess {
  /** All of them. */
  Full,

  /** Only those the user picked for the app (Android 14's "selected photos"): they can add more. */
  Partial,
  None;

  companion object {
    fun of(context: Context): MediaAccess {
      fun granted(permission: String) = ContextCompat.checkSelfPermission(context, permission) == PackageManager.PERMISSION_GRANTED
      return when {
        Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
          granted(Manifest.permission.READ_MEDIA_IMAGES) -> Full
        Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU &&
          granted(Manifest.permission.READ_EXTERNAL_STORAGE) -> Full
        Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE &&
          granted(Manifest.permission.READ_MEDIA_VISUAL_USER_SELECTED) -> Partial
        else -> None
      }
    }

    /** What to ask for: photos and videos, and on Android 14 the choice of only some of them. */
    fun permissions(): Array<String> = when {
      Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE -> arrayOf(
        Manifest.permission.READ_MEDIA_IMAGES,
        Manifest.permission.READ_MEDIA_VIDEO,
        Manifest.permission.READ_MEDIA_VISUAL_USER_SELECTED,
      )
      Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU -> arrayOf(
        Manifest.permission.READ_MEDIA_IMAGES,
        Manifest.permission.READ_MEDIA_VIDEO,
      )
      else -> arrayOf(Manifest.permission.READ_EXTERNAL_STORAGE)
    }
  }
}

/** One of the device's photos or videos. */
data class RecentMediaItem(val uri: Uri, val isVideo: Boolean, val durationMs: Long)

/**
 * The newest photos and videos the app may read, newest first. Read from the device only; nothing
 * leaves it until the user adds one to a message.
 */
suspend fun recentMedia(context: Context, limit: Int = RECENT_MEDIA_LIMIT): List<RecentMediaItem> = withContext(Dispatchers.IO) {
  val collection = MediaStore.Files.getContentUri("external")
  val projection = arrayOf(MediaStore.Files.FileColumns._ID, MediaStore.Files.FileColumns.MEDIA_TYPE, MediaStore.Video.VideoColumns.DURATION)
  val selection = "${MediaStore.Files.FileColumns.MEDIA_TYPE} IN (?, ?)"
  val arguments = arrayOf(
    MediaStore.Files.FileColumns.MEDIA_TYPE_IMAGE.toString(),
    MediaStore.Files.FileColumns.MEDIA_TYPE_VIDEO.toString(),
  )
  val order = "${MediaStore.Files.FileColumns.DATE_ADDED} DESC"
  runCatching {
    context.contentResolver.query(collection, projection, selection, arguments, order)?.use { cursor ->
      val idColumn = cursor.getColumnIndexOrThrow(MediaStore.Files.FileColumns._ID)
      val typeColumn = cursor.getColumnIndexOrThrow(MediaStore.Files.FileColumns.MEDIA_TYPE)
      val durationColumn = cursor.getColumnIndex(MediaStore.Video.VideoColumns.DURATION)
      buildList {
        while (cursor.moveToNext() && size < limit) {
          val id = cursor.getLong(idColumn)
          val isVideo = cursor.getInt(typeColumn) == MediaStore.Files.FileColumns.MEDIA_TYPE_VIDEO
          val base = if (isVideo) MediaStore.Video.Media.EXTERNAL_CONTENT_URI else MediaStore.Images.Media.EXTERNAL_CONTENT_URI
          val duration = if (isVideo && durationColumn >= 0) cursor.getLong(durationColumn) else 0L
          add(RecentMediaItem(ContentUris.withAppendedId(base, id), isVideo, duration))
        }
      }
    }
  }.getOrNull().orEmpty()
}

/** A small square picture of an item for the strip; `null` when the device can't make one. */
suspend fun recentMediaThumbnail(context: Context, item: RecentMediaItem, sidePx: Int): Bitmap? = withContext(Dispatchers.IO) {
  runCatching {
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
      context.contentResolver.loadThumbnail(item.uri, Size(sidePx, sidePx), null)
    } else if (!item.isVideo) {
      val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
      context.contentResolver.openInputStream(item.uri)?.use { BitmapFactory.decodeStream(it, null, bounds) }
      val options = BitmapFactory.Options().apply { inSampleSize = sampleSize(bounds.outWidth, bounds.outHeight, sidePx) }
      context.contentResolver.openInputStream(item.uri)?.use { BitmapFactory.decodeStream(it, null, options) }
    } else {
      null
    }
  }.getOrNull()
}
