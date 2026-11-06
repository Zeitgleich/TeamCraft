package de.zeitgleich.teamcraft.ui.media

import android.content.ContentUris
import android.content.Context
import android.net.Uri
import android.os.Build
import android.os.Environment
import android.provider.MediaStore
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import de.zeitgleich.teamcraft.ui.RecentMediaItem

/** An album of the gallery: its cover (the newest item) and how many it holds. */
data class MediaAlbum(val ref: MediaAlbumRef, val cover: RecentMediaItem, val count: Int)

/**
 * The device's photos and videos by album, read from MediaStore on the device. Only what the app
 * may read shows (all of it, or Android 14's "selected photos").
 */
object MediaLibrary {
  private val collection: Uri get() = MediaStore.Files.getContentUri("external")
  /** Where the file is: the relative path from Android 10, the full path before. */
  @Suppress("DEPRECATION")
  private val pathColumnName: String =
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) MediaStore.MediaColumns.RELATIVE_PATH else MediaStore.MediaColumns.DATA
  private val projection = arrayOf(
    MediaStore.Files.FileColumns._ID,
    MediaStore.Files.FileColumns.MEDIA_TYPE,
    MediaStore.Video.VideoColumns.DURATION,
    BUCKET_ID,
    BUCKET_NAME,
    pathColumnName,
  )

  /**
   * The albums: Camera first, then all media, then the device's folders by their newest item. One
   * pass over the IDs (no pixels); a large library takes a moment, so it runs off the main thread.
   */
  suspend fun albums(context: Context): List<MediaAlbum> = withContext(Dispatchers.IO) {
    var camera: MediaAlbum? = null
    var maybeCameraBucket: Long? = null
    var all: MediaAlbum? = null
    val folders = LinkedHashMap<Long, MediaAlbum>()
    query(context, null, null) { item, bucketId, bucketName, isCamera ->
      all = all?.let { it.copy(count = it.count + 1) } ?: MediaAlbum(MediaAlbumRef(MediaAlbumKind.All), item, 1)
      if (isCamera) {
        maybeCameraBucket = bucketId
        camera = camera?.let { it.copy(count = it.count + 1) } ?: MediaAlbum(MediaAlbumRef(MediaAlbumKind.Camera), item, 1)
      }
      val folder = folders[bucketId]
      folders[bucketId] = folder?.copy(count = folder.count + 1)
        ?: MediaAlbum(MediaAlbumRef(MediaAlbumKind.Folder, bucketId, bucketName), item, 1)
      true
    }
    // The camera's folder is already the first album.
    listOfNotNull(camera, all) + folders.values.filterNot { it.ref.maybeBucketId == maybeCameraBucket }
  }

  /** An album's photos and videos, newest first. */
  suspend fun items(context: Context, album: MediaAlbumRef): List<RecentMediaItem> = withContext(Dispatchers.IO) {
    val items = ArrayList<RecentMediaItem>()
    val (selection, arguments) = when (album.kind) {
      MediaAlbumKind.Folder -> "$BUCKET_ID = ?" to arrayOf(album.maybeBucketId.toString())
      else -> null to null
    }
    query(context, selection, arguments) { item, _, _, isCamera ->
      if (album.kind != MediaAlbumKind.Camera || isCamera) items += item
      true
    }
    items
  }

  /** The newest photo or video, for the camera's gallery button. */
  suspend fun newest(context: Context): RecentMediaItem? = withContext(Dispatchers.IO) {
    var newest: RecentMediaItem? = null
    query(context, null, null) { item, _, _, _ ->
      newest = item
      false
    }
    newest
  }

  /**
   * Walks the photos and videos newest first; `visit` gets each with its folder and whether it is
   * the camera's, and returns whether to go on.
   */
  private fun query(
    context: Context,
    extraSelection: String?,
    extraArguments: Array<String>?,
    visit: (RecentMediaItem, Long, String, Boolean) -> Boolean,
  ) {
    val typeSelection = "${MediaStore.Files.FileColumns.MEDIA_TYPE} IN (?, ?)"
    val selection = listOfNotNull(typeSelection, extraSelection).joinToString(" AND ")
    val arguments = arrayOf(
      MediaStore.Files.FileColumns.MEDIA_TYPE_IMAGE.toString(),
      MediaStore.Files.FileColumns.MEDIA_TYPE_VIDEO.toString(),
    ) + extraArguments.orEmpty()
    val order = "${MediaStore.Files.FileColumns.DATE_ADDED} DESC"
    runCatching {
      context.contentResolver.query(collection, projection, selection, arguments, order)?.use { cursor ->
        val idColumn = cursor.getColumnIndexOrThrow(MediaStore.Files.FileColumns._ID)
        val typeColumn = cursor.getColumnIndexOrThrow(MediaStore.Files.FileColumns.MEDIA_TYPE)
        val durationColumn = cursor.getColumnIndex(MediaStore.Video.VideoColumns.DURATION)
        val bucketColumn = cursor.getColumnIndex(BUCKET_ID)
        val nameColumn = cursor.getColumnIndex(BUCKET_NAME)
        val pathColumn = cursor.getColumnIndex(pathColumnName)
        while (cursor.moveToNext()) {
          val id = cursor.getLong(idColumn)
          val isVideo = cursor.getInt(typeColumn) == MediaStore.Files.FileColumns.MEDIA_TYPE_VIDEO
          val base = if (isVideo) MediaStore.Video.Media.EXTERNAL_CONTENT_URI else MediaStore.Images.Media.EXTERNAL_CONTENT_URI
          val duration = if (isVideo && durationColumn >= 0) cursor.getLong(durationColumn) else 0L
          val bucketId = if (bucketColumn >= 0) cursor.getLong(bucketColumn) else 0L
          val bucketName = if (nameColumn >= 0) cursor.getString(nameColumn).orEmpty() else ""
          val maybePath = if (pathColumn >= 0) cursor.getString(pathColumn) else null
          val item = RecentMediaItem(ContentUris.withAppendedId(base, id), isVideo, duration)
          if (!visit(item, bucketId, bucketName, isCameraFolder(maybePath))) break
        }
      }
    }
  }

  /** The camera's own folder, `DCIM/Camera/` (the same on every vendor the app was tried on). */
  private fun isCameraFolder(maybePath: String?): Boolean {
    val folder = maybePath?.let { if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) it.trimEnd('/') else it.substringBeforeLast('/') }
    return folder?.endsWith("${Environment.DIRECTORY_DCIM}/Camera", ignoreCase = true) == true
  }

  // The same columns on every version (MediaColumns has the constants only from Android 10).
  private const val BUCKET_ID = "bucket_id"
  private const val BUCKET_NAME = "bucket_display_name"
}
