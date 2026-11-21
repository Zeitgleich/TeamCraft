package de.zeitgleich.teamcraft.ui

import android.graphics.Bitmap
import android.media.MediaMetadataRetriever
import java.io.ByteArrayOutputStream
import de.zeitgleich.teamcraft.core.PickedFile

/** Longest side of a video's poster frame: what senders make for pictures too (`Pictures.kt`). */
private const val POSTER_SIDE_PX = 640
private const val POSTER_QUALITY = 80

/** Where the poster frame is taken: about a second in, past a black first frame. */
private const val POSTER_AT_US = 1_000_000L

/**
 * [file] with the poster frame and length of its video, read on the device, so the conversation
 * shows a video tile instead of a file card (the core has no video decoder; the browser sends the
 * same, `raster::video_poster`). Unchanged when it is no video or the device can't read it.
 */
fun withVideoPoster(file: PickedFile): PickedFile {
  if (!file.mediaType.startsWith("video/")) return file
  val retriever = MediaMetadataRetriever()
  return try {
    retriever.setDataSource(file.path)
    val maybeDuration = retriever.extractMetadata(MediaMetadataRetriever.METADATA_KEY_DURATION)?.toLongOrNull()
    val at = maybeDuration?.let { minOf(POSTER_AT_US, it * 1_000 / 2) } ?: 0L
    val maybeFrame = retriever.getScaledFrameAtTime(at, MediaMetadataRetriever.OPTION_CLOSEST_SYNC, POSTER_SIDE_PX, POSTER_SIDE_PX)
    file.copy(maybeThumbnail = maybeFrame?.let(::jpeg), maybeDurationMs = maybeDuration)
  } catch (failure: RuntimeException) {
    // Not readable here (a codec the device lacks): it goes as a file card, as before.
    file
  } finally {
    retriever.release()
  }
}

/** Pictures the core's own decoder can't read but the phone's may: they get their thumbnail here. */
internal fun needsPictureThumbnail(mediaType: String, sdk: Int = android.os.Build.VERSION.SDK_INT): Boolean =
  mediaTypeEssence(mediaType) in setOf("image/avif", "image/heic", "image/heif", "image/heic-sequence", "image/heif-sequence") &&
    canDecodeHere(mediaType, sdk)

/**
 * [file] with a thumbnail of its picture when it is an AVIF or HEIC: the core can't decode those, so
 * without this the conversation would show them as file cards on every device that doesn't
 * decode the whole file. PNG, so transparency stays. Unchanged when the phone can't read it either.
 */
fun withPictureThumbnail(file: PickedFile): PickedFile {
  if (file.maybeThumbnail != null || !needsPictureThumbnail(file.mediaType)) return file
  val bitmap = Pictures.decodeUpright(file.path, POSTER_SIDE_PX) ?: return file
  val bytes = ByteArrayOutputStream().use { out ->
    bitmap.compress(Bitmap.CompressFormat.PNG, 100, out)
    bitmap.recycle()
    out.toByteArray()
  }
  return file.copy(maybeThumbnail = bytes)
}

private fun jpeg(frame: Bitmap): ByteArray = ByteArrayOutputStream().use { out ->
  frame.compress(Bitmap.CompressFormat.JPEG, POSTER_QUALITY, out)
  frame.recycle()
  out.toByteArray()
}
