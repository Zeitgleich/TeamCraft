package de.zeitgleich.teamcraft.ui.media

import java.io.File
import java.nio.file.Files
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class MediaDraftStoreTest {
  @Test
  fun theEndOfASessionTakesTheCameraFilesOfEveryAccount() {
    val files = files()
    val first = capture(files, "server-a", "account-1", "one")
    val second = capture(files, "server-b", "account-2", "two")
    val upload = File(first.parentFile, "123-photo.jpg").apply { writeText("staged") }
    MediaDraftStore.deleteAll(files)
    assertFalse(first.exists())
    assertFalse(second.exists())
    assertFalse(File(files, "media-flow").exists())
    // Files the core staged for sending are the core's: its own sweep and sign-out handle them.
    assertTrue(upload.exists())
  }

  @Test
  fun theSweepRemovesCameraFilesWhoseDraftIsGone() {
    val files = files()
    val kept = capture(files, "server-a", "account-1", "kept")
    val orphan = capture(files, "server-a", "account-1", "orphan")
    File(files, "media-flow").mkdirs()
    File(files, "media-flow/draft-kept.sealed").writeText("sealed")
    MediaDraftStore.sweep(files, nowMillis = System.currentTimeMillis())
    assertTrue(kept.exists())
    assertFalse(orphan.exists())
  }

  @Test
  fun aWeekOldDraftGoesWithItsCameraFiles() {
    val files = files()
    val captures = capture(files, "server-a", "account-1", "old")
    File(files, "media-flow").mkdirs()
    File(files, "media-flow/draft-old.sealed").apply { writeText("sealed") }
    MediaDraftStore.sweep(files, nowMillis = System.currentTimeMillis() + 8L * 24 * 60 * 60 * 1000)
    assertFalse(File(files, "media-flow/draft-old.sealed").exists())
    assertFalse(captures.exists())
  }

  private fun files(): File = Files.createTempDirectory("media-draft").toFile().apply { deleteOnExit() }

  private fun capture(files: File, server: String, account: String, key: String): File =
    File(files, "store/$server/outgoing/$account/media-flow-captures-$key").apply {
      mkdirs()
      File(this, "photo.jpg").writeText("picture")
    }
}
