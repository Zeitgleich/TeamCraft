package de.zeitgleich.teamcraft.ui

import android.media.MediaMetadataRetriever
import android.util.Log
import java.io.File
import de.zeitgleich.teamcraft.core.PickedFile
import de.zeitgleich.teamcraft.core.PickedMotion
import uniffi.teamcraft_ffi.MotionLimitsItem
import uniffi.teamcraft_ffi.TeamCraftException
import uniffi.teamcraft_ffi.splitMotionPhoto

private const val TAG = "MotionPhotos"

/**
 * [file] as a picture and its motion when it is a Motion Photo (#2181): the camera put an MP4 behind
 * the JPEG, which the core finds and cuts out (`motion_photo`, it checks every length against the
 * file). The picture then goes without the video, and the video beside it as its motion part.
 *
 * Unchanged when the user sends pictures still ([isSendingMotion]), the server doesn't allow motion,
 * it isn't a JPEG or there is no video in it. A clip over the server's size goes along anyway: the
 * core sends the picture alone and shows the sender that the motion wasn't sent.
 */
fun withMotion(file: PickedFile, maybeLimits: MotionLimitsItem?, isSendingMotion: Boolean): PickedFile {
  if (!isSendingMotion || maybeLimits?.isAllowed != true || file.maybeMotion != null || !isJpeg(file.mediaType)) return file
  val source = File(file.path)
  val directory = source.parentFile ?: return file
  val stamp = System.nanoTime()
  val still = File(directory, "$stamp-still-${source.name}")
  val video = File(directory, "$stamp-motion.mp4")
  val maybeSplit = try {
    splitMotionPhoto(source.path, still.path, video.path)
  } catch (failure: TeamCraftException) {
    Log.d(TAG, "not read as a motion photo", failure)
    null
  }
  if (maybeSplit == null) {
    still.delete()
    video.delete()
    return file
  }
  // The staged copy with the video inside is no longer needed: the picture goes without it.
  source.delete()
  return file.copy(path = still.path, maybeMotion = PickedMotion(video.path, "video/mp4", durationOf(video)))
}

private fun isJpeg(mediaType: String): Boolean = mediaType.substringBefore(';').trim().lowercase() in setOf("image/jpeg", "image/jpg")

/** The clip's length, read by the device; `null` when it can't read it. */
private fun durationOf(video: File): Long? {
  val retriever = MediaMetadataRetriever()
  return try {
    retriever.setDataSource(video.path)
    retriever.extractMetadata(MediaMetadataRetriever.METADATA_KEY_DURATION)?.toLongOrNull()
  } catch (failure: RuntimeException) {
    null
  } finally {
    retriever.release()
  }
}
