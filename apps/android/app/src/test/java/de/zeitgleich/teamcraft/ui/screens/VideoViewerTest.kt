package de.zeitgleich.teamcraft.ui.screens

import androidx.media3.common.MimeTypes
import androidx.media3.common.PlaybackException
import de.zeitgleich.teamcraft.core.FileInfo
import de.zeitgleich.teamcraft.core.FileTransfer
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class VideoViewerTest {
  @Test
  fun aDoubleTapSeeksBackOnTheLeftAndForwardOnTheRight() {
    assertEquals(-10_000L, doubleTapSeekMs(10f, 100f))
    assertEquals(10_000L, doubleTapSeekMs(50f, 100f))
    assertEquals(10_000L, doubleTapSeekMs(90f, 100f))
  }

  @Test
  fun aFailedDeviceDecoderRetriesWithTheBundledOneOnlyForAv1AndVp9() {
    for (type in listOf(MimeTypes.VIDEO_AV1, MimeTypes.VIDEO_VP9)) {
      assertTrue(type, canRetryWithBundledDecoder(PlaybackException.ERROR_CODE_DECODER_INIT_FAILED, type))
      assertTrue(type, canRetryWithBundledDecoder(PlaybackException.ERROR_CODE_DECODING_FAILED, type))
      assertTrue(type, canRetryWithBundledDecoder(PlaybackException.ERROR_CODE_DECODING_FORMAT_UNSUPPORTED, type))
    }
    // No bundled decoder for these: HEVC stays with the hardware, VP8 every device decodes.
    for (type in listOf(MimeTypes.VIDEO_H265, MimeTypes.VIDEO_VP8, MimeTypes.VIDEO_H264, null)) {
      assertFalse("$type", canRetryWithBundledDecoder(PlaybackException.ERROR_CODE_DECODING_FAILED, type))
    }
    // A broken file isn't a decoder's fault.
    assertFalse(canRetryWithBundledDecoder(PlaybackException.ERROR_CODE_PARSING_CONTAINER_MALFORMED, MimeTypes.VIDEO_AV1))
    assertFalse(canRetryWithBundledDecoder(PlaybackException.ERROR_CODE_IO_FILE_NOT_FOUND, MimeTypes.VIDEO_VP9))
  }

  @Test
  fun lengthsReadLikeAClock() {
    assertEquals("0:07", formatVideoDuration(7_900))
    assertEquals("12:34", formatVideoDuration(754_000))
    assertEquals("1:02:03", formatVideoDuration(3_723_000))
    assertEquals("0:00", formatVideoDuration(-5))
  }

  @Test
  fun aVideoOpensInThePlayerUnlessItIsStillGoingUp() {
    for (state in listOf("local", "remote", "downloading", "failed")) {
      assertTrue(state, file("video/webm", state).opensInVideoPlayer)
    }
    for (state in listOf("uploading", "upload_stalled", "upload_failed")) {
      assertFalse(state, file("video/mp4", state).opensInVideoPlayer)
    }
    assertFalse(file("image/png", "local").opensInVideoPlayer)
    assertFalse(file("application/pdf", "local").opensInVideoPlayer)
  }

  private fun file(mediaType: String, state: String) = FileInfo(
    conversation = "1",
    message = "2",
    fileId = "3",
    fileName = "clip",
    mediaType = mediaType,
    sizeBytes = 1,
    transfer = FileTransfer(state = state),
  )
}
