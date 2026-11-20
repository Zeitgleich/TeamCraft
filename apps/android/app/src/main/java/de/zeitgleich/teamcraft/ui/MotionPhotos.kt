package de.zeitgleich.teamcraft.ui

import android.content.Context
import android.media.MediaExtractor
import android.media.MediaFormat
import android.media.MediaMetadataRetriever
import android.util.Log
import androidx.annotation.OptIn
import androidx.media3.common.MediaItem
import androidx.media3.common.MimeTypes
import androidx.media3.common.util.UnstableApi
import androidx.media3.transformer.Composition
import androidx.media3.transformer.EditedMediaItem
import androidx.media3.transformer.ExportException
import androidx.media3.transformer.ExportResult
import androidx.media3.transformer.Transformer
import java.io.File
import kotlin.coroutines.resume
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.withContext
import de.zeitgleich.teamcraft.core.PickedFile
import de.zeitgleich.teamcraft.core.PickedMotion
import uniffi.teamcraft_ffi.MotionLimitsItem
import uniffi.teamcraft_ffi.TeamCraftException
import uniffi.teamcraft_ffi.splitMotionPhoto

private const val TAG = "MotionPhotos"

/**
 * [file] as a picture and its motion when it is a Motion Photo (#2181): the camera put an MP4 behind
 * the JPEG, which the core finds and cuts out (`motion_photo`, it checks every length against the
 * file). The picture then goes without the video, and the video beside it as its motion part,
 * converted to H.264 when the camera wrote HEVC, so every browser plays it.
 *
 * Unchanged when the user sends pictures still ([isSendingMotion]), the server doesn't allow motion,
 * it isn't a JPEG or there is no video in it. A clip over the server's size goes along unconverted:
 * the core sends the picture alone and shows the sender that the motion wasn't sent.
 */
suspend fun withMotion(context: Context, file: PickedFile, maybeLimits: MotionLimitsItem?, isSendingMotion: Boolean): PickedFile {
  if (!isSendingMotion || maybeLimits?.isAllowed != true || file.maybeMotion != null || !isJpeg(file.mediaType)) return file
  val source = File(file.path)
  val directory = source.parentFile ?: return file
  val stamp = System.nanoTime()
  val still = File(directory, "$stamp-still-${source.name}")
  val video = File(directory, "$stamp-motion.mp4")
  val maybeSplit = withContext(Dispatchers.IO) {
    try {
      splitMotionPhoto(source.path, still.path, video.path)
    } catch (failure: TeamCraftException) {
      Log.d(TAG, "not read as a motion photo", failure)
      null
    }
  }
  if (maybeSplit == null) {
    still.delete()
    video.delete()
    return file
  }
  // The staged copy with the video inside is no longer needed: the picture goes without it.
  source.delete()
  val clip = if (maybeSplit.videoBytes <= maybeLimits.maxBytes) playableEverywhere(context, video) else video
  return file.copy(path = still.path, maybeMotion = PickedMotion(clip.path, "video/mp4", durationOf(clip)))
}

private fun isJpeg(mediaType: String): Boolean = mediaType.substringBefore(';').trim().lowercase() in setOf("image/jpeg", "image/jpg")

/**
 * [clip] as H.264 (which every browser and phone decodes), converted beside it when it is anything
 * else (cameras write HEVC); the clip as it is when it is H.264 already or the conversion fails, in
 * which case receivers that can't decode it show the still picture. Sound is left out: the motion
 * always plays muted.
 */
private suspend fun playableEverywhere(context: Context, clip: File): File {
  if (videoMimeType(clip) == MimeTypes.VIDEO_H264) return clip
  val target = File(clip.parentFile, clip.nameWithoutExtension + "-h264.mp4")
  val isConverted = withContext(Dispatchers.Main) { convertToH264(context, clip, target) }
  if (!isConverted) {
    target.delete()
    return clip
  }
  clip.delete()
  return target
}

@OptIn(UnstableApi::class)
private suspend fun convertToH264(context: Context, source: File, target: File): Boolean {
  val edited = EditedMediaItem.Builder(MediaItem.fromUri(android.net.Uri.fromFile(source))).setRemoveAudio(true).build()
  return suspendCancellableCoroutine { continuation ->
    val transformer = Transformer.Builder(context)
      .setVideoMimeType(MimeTypes.VIDEO_H264)
      .addListener(object : Transformer.Listener {
        override fun onCompleted(composition: Composition, exportResult: ExportResult) {
          if (continuation.isActive) continuation.resume(true)
        }

        override fun onError(composition: Composition, exportResult: ExportResult, exportException: ExportException) {
          // The code only: the path names the user's file.
          Log.i(TAG, "motion not converted: ${exportException.errorCodeName}")
          if (continuation.isActive) continuation.resume(false)
        }
      })
      .build()
    continuation.invokeOnCancellation {
      transformer.cancel()
      target.delete()
    }
    transformer.start(edited, target.absolutePath)
  }
}

/** The MIME type of the clip's video track, `null` when it can't be read. */
private suspend fun videoMimeType(clip: File): String? = withContext(Dispatchers.IO + NonCancellable) {
  val extractor = MediaExtractor()
  try {
    extractor.setDataSource(clip.path)
    (0 until extractor.trackCount)
      .map { extractor.getTrackFormat(it).getString(MediaFormat.KEY_MIME) }
      .firstOrNull { it?.startsWith("video/") == true }
  } catch (failure: Exception) {
    null
  } finally {
    extractor.release()
  }
}

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
