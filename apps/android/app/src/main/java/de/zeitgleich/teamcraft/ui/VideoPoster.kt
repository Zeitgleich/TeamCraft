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

private fun jpeg(frame: Bitmap): ByteArray = ByteArrayOutputStream().use { out ->
  frame.compress(Bitmap.CompressFormat.JPEG, POSTER_QUALITY, out)
  frame.recycle()
  out.toByteArray()
}
