package de.zeitgleich.teamcraft.ui

import java.io.ByteArrayInputStream
import java.io.File
import java.nio.file.Files
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

private const val NOW_MS = 1_800_000_000_000L
private const val DAY_MS = 24L * 60 * 60 * 1000

class ShareInboxTest {
  private val root: File = Files.createTempDirectory("share-inbox").toFile()
  private val inbox = ShareInbox(root, maxFileBytes = 10)

  @After
  fun cleanUp() {
    root.deleteRecursively()
  }

  @Test
  fun aShareSurvivesARestart() {
    val share = inbox.stage(content("hello"), listOf(source("a.png", ByteArray(3))), NOW_MS)

    val restored = ShareInbox(root, maxFileBytes = 10).oldest()

    assertEquals(share, restored)
    assertEquals(3L, inbox.fileOf(share, share.files[0]).length())
    assertEquals("a.png", share.files[0].name)
  }

  @Test
  fun aSendersNameNeverBecomesAPath() {
    val share = inbox.stage(content(""), listOf(source("../../secret", ByteArray(1))), NOW_MS)
    val stored = inbox.fileOf(share, share.files[0])
    assertEquals(File(root, share.id).canonicalPath, stored.canonicalFile.parentFile?.path)
    assertEquals("../../secret", share.files[0].name)
  }

  @Test
  fun filesThatFailCountAsLeftOut() {
    val sources = listOf(
      source("big", ByteArray(11)),
      ShareSource("gone", "text/plain") { null },
      ShareSource("broken", "text/plain") { throw SecurityException("no grant") },
      source("fine", ByteArray(10)),
    )
    val share = inbox.stage(content("", leftOut = 2), sources, NOW_MS)
    assertEquals(listOf("fine"), share.files.map { it.name })
    assertEquals(5, share.leftOut)
    assertEquals(1, File(root, share.id).listFiles()!!.count { it.name != "share.json" })
  }

  @Test
  fun theOldestComesFirstAndHandledOnesGo() {
    val first = inbox.stage(content("first"), emptyList(), NOW_MS)
    val second = inbox.stage(content("second"), emptyList(), NOW_MS + 1)

    assertEquals(first.id, inbox.oldest()?.id)
    assertEquals(second.id, inbox.oldest(except = setOf(first.id))?.id)
    inbox.delete(first.id)
    assertEquals(second.id, inbox.oldest()?.id)
    inbox.wipe()
    assertNull(inbox.oldest())
  }

  @Test
  fun pruningDropsOldSharesAndCopiesCutShort() {
    val old = inbox.stage(content("old"), emptyList(), NOW_MS - 8 * DAY_MS)
    val recent = inbox.stage(content("recent"), emptyList(), NOW_MS - DAY_MS)
    val cutShort = File(root, "${NOW_MS - 2 * 60 * 60 * 1000}-a").apply { mkdirs() }
    val stillCopying = File(root, "${NOW_MS - 1000}-b").apply { mkdirs() }

    inbox.prune(NOW_MS)

    assertFalse(File(root, old.id).exists())
    assertTrue(File(root, recent.id).exists())
    assertFalse(cutShort.exists())
    assertTrue(stillCopying.exists())
  }

  @Test
  fun twoSharesInTheSameMillisecondStayApart() {
    assertNotEquals(inbox.stage(content("a"), emptyList(), NOW_MS).id, inbox.stage(content("b"), emptyList(), NOW_MS).id)
  }

  private fun content(text: String, leftOut: Int = 0) = SharedContent(text, emptyList(), leftOut = leftOut)

  private fun source(name: String, bytes: ByteArray) =
    ShareSource(name, "application/octet-stream") { ByteArrayInputStream(bytes) }
}
