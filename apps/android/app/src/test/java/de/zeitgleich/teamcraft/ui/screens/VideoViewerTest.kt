package de.zeitgleich.teamcraft.ui.screens

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
