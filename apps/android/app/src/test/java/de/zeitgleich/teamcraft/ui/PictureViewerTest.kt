package de.zeitgleich.teamcraft.ui

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import uniffi.teamcraft_ffi.OriginalDownload

/** The image viewer's own pieces on the app side (#240); paging and albums are tested in the core. */
class PictureViewerTest {
  @Test
  fun theOpenedPictureSurvivesASavedScreen() {
    val target = PictureTarget(conversation = "7", message = "10", fileId = "3")
    assertEquals(target, PictureTarget.fromList(target.toList()))
    assertEquals("10:3", target.key)
    assertNull(PictureTarget.fromList(listOf("7", "10")))
  }

  @Test
  fun theOriginalDownloadSettingReadsBackAndDefaultsToAlways() {
    assertEquals(OriginalDownload.UNMETERED_ONLY, originalDownloadOf(OriginalDownload.UNMETERED_ONLY.name))
    assertEquals(OriginalDownload.NEVER, originalDownloadOf("NEVER"))
    assertEquals(OriginalDownload.ALWAYS, originalDownloadOf(null))
    assertEquals(OriginalDownload.ALWAYS, originalDownloadOf("sometimes"))
  }
}
