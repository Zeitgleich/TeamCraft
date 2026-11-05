package de.zeitgleich.teamcraft.ui

import java.io.ByteArrayInputStream
import java.io.File
import java.io.InputStream
import java.nio.file.Files
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
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
  private val inbox = ShareInbox(root, maxFileBytes = 10, maxShares = 3)

  @After
  fun cleanUp() {
    root.deleteRecursively()
  }

  @Test
  fun aShareSurvivesARestart() {
    val share = stored(content("hello"), listOf(source("a.png", ByteArray(3))), NOW_MS)

    val restored = ShareInbox(root, maxFileBytes = 10).oldest()

    assertEquals(share, restored)
    assertEquals(3L, inbox.fileOf(share, share.files[0]).length())
    assertEquals("a.png", share.files[0].name)
  }

  @Test
  fun aSendersNameNeverBecomesAPath() {
    val share = stored(content(""), listOf(source("../../secret", ByteArray(1))), NOW_MS)
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
    val share = stored(content("", leftOut = 2), sources, NOW_MS)
    assertEquals(listOf("fine"), share.files.map { it.name })
    assertEquals(5, share.leftOut)
    assertEquals(1, File(root, share.id).listFiles()!!.count { it.name != "share.json" })
  }

  @Test
  fun theOldestComesFirstAndHandledOnesGo() {
    val first = stored(content("first"), emptyList(), NOW_MS)
    val second = stored(content("second"), emptyList(), NOW_MS + 1)

    assertEquals(first.id, inbox.oldest()?.id)
    assertEquals(second.id, inbox.oldest(except = setOf(first.id))?.id)
    inbox.delete(first.id)
    assertEquals(second.id, inbox.oldest()?.id)
    inbox.wipe()
    assertNull(inbox.oldest())
  }

  @Test
  fun pruningDropsOldSharesAndCopiesCutShort() {
    val old = stored(content("old"), emptyList(), NOW_MS - 8 * DAY_MS)
    val recent = stored(content("recent"), emptyList(), NOW_MS - DAY_MS)
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
    assertNotEquals(stored(content("a"), emptyList(), NOW_MS).id, stored(content("b"), emptyList(), NOW_MS).id)
  }

  @Test
  fun cancellingDuringACopyLeavesNothing() {
    var reads = 0
    // Says "cancelled" once the copy has started reading: the user discarded while it ran.
    val isCancelled = { reads > 0 }
    val stream = object : ByteArrayInputStream(ByteArray(5)) {
      override fun read(buffer: ByteArray): Int {
        reads++
        return super.read(buffer)
      }
    }
    val result = inbox.stage(content("text"), listOf(ShareSource("a", "text/plain") { stream }), NOW_MS, isCancelled)

    assertEquals(ShareStageResult.Cancelled, result)
    assertEquals(1, reads)
    assertNull(inbox.oldest())
    assertEquals(0, root.listFiles()!!.size)
  }

  @Test
  fun cancellingAfterTheLastFileStillWritesNoList() {
    var isDone = false
    val source = ShareSource("a", "text/plain") {
      isDone = true
      ByteArrayInputStream(ByteArray(0))
    }
    // True only after the file was opened: the copy loop of an empty file saw no cancellation yet.
    val result = inbox.stage(content("text"), listOf(source), NOW_MS, isCancelled = { isDone })
    assertEquals(ShareStageResult.Cancelled, result)
    assertEquals(0, root.listFiles()!!.size)
  }

  @Test
  fun aFurtherShareThanAllowedFailsWithNothingLeftBehind() {
    repeat(3) { stored(content("share $it"), emptyList(), NOW_MS + it) }
    assertEquals(ShareStageResult.Failed, inbox.stage(content("one too many"), emptyList(), NOW_MS + 9))
    assertEquals(3, root.listFiles()!!.size)
  }

  @Test
  fun filesBeyondTheInboxsBytesAreLeftOut() {
    val small = ShareInbox(root, maxFileBytes = 1000, maxTotalBytes = 1000)
    small.stage(content("first"), listOf(source("first", ByteArray(600))), NOW_MS)
    val result = small.stage(
      content("second"),
      // What is left is 400 bytes less the first share's list.
      listOf(source("too much", ByteArray(400)), source("fits", ByteArray(100))),
      NOW_MS + 1,
    )
    val share = (result as ShareStageResult.Stored).share
    assertEquals(listOf("fits"), share.files.map { it.name })
    assertEquals(1, share.leftOut)
  }

  @Test
  fun aListThatCantBeWrittenIsAFailureWithoutACrash() {
    // A file where the inbox's folder belongs: the folder can't be made, as on a full or broken disk.
    val blocked = File(root, "blocked").apply { writeText("") }
    val inboxOnFile = ShareInbox(blocked, maxFileBytes = 10)
    assertEquals(ShareStageResult.Failed, inboxOnFile.stage(content("text"), emptyList(), NOW_MS))
  }

  @Test
  fun whatFailedStaysWithoutTheTextAndTheShortcut() {
    val share = stored(
      SharedContent("text", emptyList(), maybeTarget = "42"),
      listOf(source("a", ByteArray(1)), source("b", ByteArray(1))),
      NOW_MS,
    )
    inbox.keepOnly(share, listOf(share.files[1]))
    val kept = inbox.oldest()!!
    assertEquals(listOf("b"), kept.files.map { it.name })
    assertEquals("", kept.text)
    assertNull(kept.maybeTarget)
  }

  @Test
  fun aBlockedCopyDoesntHoldUpTheStopAndLeavesNothingWhenItEnds() = runBlocking {
    val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    val receiver = ShareReceiver(root, maxFileBytes = 1000, scope)
    val isReading = CountDownLatch(1)
    val release = CountDownLatch(1)
    val blocked = object : InputStream() {
      override fun read(): Int = throw AssertionError("reads arrays")

      override fun read(buffer: ByteArray): Int {
        isReading.countDown()
        release.await()
        return 1
      }
    }
    receiver.receive(content("text")) { listOf(ShareSource("a", "text/plain") { blocked }) }
    val copy = scope.coroutineContext[Job]!!.children.single()
    assertTrue(isReading.await(10, TimeUnit.SECONDS))

    withTimeout(10_000) { receiver.stopCopies(timeoutMs = 1) }
    receiver.inbox.wipe()
    assertTrue("still blocked in the provider", copy.isActive || copy.isCancelled)
    release.countDown()
    copy.join()

    assertNull(receiver.inbox.oldest())
    assertEquals(0, root.listFiles().orEmpty().size)
    scope.cancel()
  }

  @Test
  fun aCopyBlockedBeforeItBeganNeverStarts() = runBlocking {
    val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    val receiver = ShareReceiver(root, maxFileBytes = 1000, scope)
    val isAsking = CountDownLatch(1)
    val release = CountDownLatch(1)
    // A provider stuck in `query`, before the copy has its list of files.
    receiver.receive(content("text")) {
      isAsking.countDown()
      release.await()
      emptyList()
    }
    val copy = scope.coroutineContext[Job]!!.children.single()
    assertTrue(isAsking.await(10, TimeUnit.SECONDS))
    receiver.stopCopies(timeoutMs = 1)
    release.countDown()
    copy.join()
    assertEquals(0, root.listFiles().orEmpty().size)
    scope.cancel()
  }

  @Test
  fun theSameShareDeliveredAgainIsOfferedOnce() {
    val first = stored(content("same"), emptyList(), NOW_MS)
    val again = stored(content("same"), emptyList(), NOW_MS + 1)
    assertEquals(first.id, again.id)
    assertEquals(1, root.listFiles()!!.size)
  }

  @Test
  fun aShareStaysForTheSamePersonAndGoesForAnother() {
    assertFalse("nobody signed in before", inbox.claim("1"))
    stored(content("mine"), emptyList(), NOW_MS)
    assertFalse(inbox.claim("1"))
    assertEquals("mine", inbox.oldest()?.text)

    assertTrue(inbox.claim("2"))
    assertNull(inbox.oldest())
    assertFalse("nothing left to lose", inbox.claim("3"))
  }

  @Test
  fun afterASignOutNobodyOwnsWhatIsSharedNext() {
    inbox.claim("1")
    inbox.wipe()
    stored(content("shared while signed out"), emptyList(), NOW_MS)
    assertFalse(inbox.claim("2"))
    assertEquals("shared while signed out", inbox.oldest()?.text)
  }

  private fun stored(content: SharedContent, sources: List<ShareSource>, nowMs: Long): StagedShare =
    (inbox.stage(content, sources, nowMs) as ShareStageResult.Stored).share

  private fun content(text: String, leftOut: Int = 0) = SharedContent(text, emptyList(), leftOut = leftOut)

  private fun source(name: String, bytes: ByteArray) =
    ShareSource(name, "application/octet-stream") { ByteArrayInputStream(bytes) }
}
