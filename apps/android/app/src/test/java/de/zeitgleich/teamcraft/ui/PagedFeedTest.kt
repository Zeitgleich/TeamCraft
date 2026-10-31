package de.zeitgleich.teamcraft.ui

import de.zeitgleich.teamcraft.core.Message
import de.zeitgleich.teamcraft.core.MessageTail
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Test

class PagedFeedTest {
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

  private fun tail(vararg messages: Message) = MessageTail(conversation = "c", messages = messages.toList(), hasOlder = true)

  private fun message(id: String) = Message(id = id, conversation = "c", author = "a", sentAt = id.toLong(), body = "text $id")
}
