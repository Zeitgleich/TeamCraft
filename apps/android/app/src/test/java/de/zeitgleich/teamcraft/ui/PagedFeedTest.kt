package de.zeitgleich.teamcraft.ui

import de.zeitgleich.teamcraft.core.HistoryGap
import de.zeitgleich.teamcraft.core.Message
import de.zeitgleich.teamcraft.core.MessageTail
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Test

class PagedFeedTest {
  @Test
  fun theServerIsOnlyAskedOnceTheStoreHasNoMoreRows() {
    assertEquals(
      OlderState(hasOlder = true, isLoading = false, isFailed = false),
      olderState(hasLocalOlder = true, isReading = false, serverHistory = ServerHistory.FAILED),
    )
    assertEquals(
      OlderState(hasOlder = true, isLoading = false, isFailed = false),
      olderState(hasLocalOlder = false, isReading = false, serverHistory = ServerHistory.AVAILABLE),
    )
    assertEquals(
      OlderState(hasOlder = true, isLoading = true, isFailed = false),
      olderState(hasLocalOlder = false, isReading = false, serverHistory = ServerHistory.LOADING),
    )
  }

  @Test
  fun aFailureShowsAndTheBeginningEndsThePaging() {
    assertEquals(
      OlderState(hasOlder = false, isLoading = false, isFailed = true),
      olderState(hasLocalOlder = false, isReading = false, serverHistory = ServerHistory.FAILED),
    )
    assertEquals(
      OlderState(hasOlder = false, isLoading = false, isFailed = false),
      olderState(hasLocalOlder = false, isReading = false, serverHistory = ServerHistory.COMPLETE),
    )
  }

  @Test
  fun theFirstReadAsksForTheNewestPage() = runBlocking {
    val reads = mutableListOf<Pair<String?, Int>>()
    val feed = PagedFeed(firstPage = 200, pageBack = 100) { maybeOldest, older ->
      reads += maybeOldest?.id to older
      tail(message("5"))
    }
    feed.reread()
    assertEquals(listOf(null to 200), reads)
  }

  @Test
  fun rereadsKeepTheOldestRowShown() = runBlocking {
    val reads = mutableListOf<Pair<String?, Int>>()
    val feed = PagedFeed(firstPage = 200, pageBack = 100) { maybeOldest, older ->
      reads += maybeOldest?.id to older
      tail(message("5"), message("6"))
    }
    feed.reread()
    feed.reread()
    assertEquals(listOf(null to 200, "5" to 0), reads)
  }

  @Test
  fun pagingBackStartsFromTheOldestRowAndMovesIt() = runBlocking {
    val reads = mutableListOf<Pair<String?, Int>>()
    var oldest = "5"
    val feed = PagedFeed(firstPage = 200, pageBack = 100) { maybeOldest, older ->
      reads += maybeOldest?.id to older
      tail(message(oldest)).also { oldest = "1" }
    }
    feed.reread()
    feed.older()
    feed.reread()
    assertEquals(listOf(null to 200, "5" to 100, "1" to 0), reads)
  }

  @Test
  fun aFailedReadKeepsTheAnchor() = runBlocking {
    val reads = mutableListOf<Pair<String?, Int>>()
    var fails = false
    val feed = PagedFeed(firstPage = 200, pageBack = 100) { maybeOldest, older ->
      reads += maybeOldest?.id to older
      if (fails) null else tail(message("5"))
    }
    feed.reread()
    fails = true
    feed.older()
    fails = false
    feed.reread()
    assertEquals(listOf(null to 200, "5" to 100, "5" to 0), reads)
  }

  @Test
  fun aHoleNearTheScreenIsReadFromTheReadersSide() {
    // Rows counted from the newest: rows 10 to 20 are on screen.
    val below = HistoryGap(after = "3", index = 4, status = ServerHistory.AVAILABLE)
    assertEquals(GapRequest(below, GapEdge.OLDER), gapToFill(listOf(5 to below), bottomVisible = 10, topVisible = 20, ahead = 40))
    val above = below.copy(after = "7")
    assertEquals(GapRequest(above, GapEdge.NEWER), gapToFill(listOf(55 to above), bottomVisible = 10, topVisible = 20, ahead = 40))
    assertEquals("at the top of the screen", GapRequest(above, GapEdge.NEWER), gapToFill(listOf(20 to above), bottomVisible = 10, topVisible = 20, ahead = 40))
    assertEquals("too far", null, gapToFill(listOf(61 to above), bottomVisible = 10, topVisible = 20, ahead = 40))
  }

  @Test
  fun theOldestHoleNearByGoesFirstAndNoneWhileARequestIsOnItsWayOrFailed() {
    val older = HistoryGap(after = "3", index = 4, status = ServerHistory.AVAILABLE)
    val newer = older.copy(after = "9")
    assertEquals(GapRequest(older, GapEdge.NEWER), gapToFill(listOf(12 to newer, 30 to older), bottomVisible = 10, topVisible = 20, ahead = 40))
    for (status in listOf(ServerHistory.LOADING, ServerHistory.FAILED)) {
      assertEquals(null, gapToFill(listOf(15 to older.copy(status = status)), bottomVisible = 10, topVisible = 20, ahead = 40))
    }
  }

  private fun tail(vararg messages: Message) = MessageTail(conversation = "c", messages = messages.toList(), hasOlder = true)

  private fun message(id: String) = Message(id = id, conversation = "c", author = "a", sentAt = id.toLong(), body = "text $id")
}
