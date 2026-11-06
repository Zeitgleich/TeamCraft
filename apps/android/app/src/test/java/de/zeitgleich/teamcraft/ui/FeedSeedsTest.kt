package de.zeitgleich.teamcraft.ui

import de.zeitgleich.teamcraft.core.Message
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.teamcraft_ffi.ConversationItem
import uniffi.teamcraft_ffi.ConversationKind

class FeedSeedsTest {
  @Test
  fun theNewestConversationsAreReadAheadUnlessKeptUpToDate() {
    val seeds = FeedSeeds()
    seeds.keep("b", seed(listOf("20")))
    seeds.keep("c", seed(listOf("29")))
    val list = ChatListState(previews = mapOf(entry("a", "10", 1L), entry("b", "20", 3L), entry("c", "30", 2L)))
    // b holds its newest message already; c doesn't; a is too old for a limit of two.
    assertEquals(listOf("c"), seeds.toReadAhead(list, limit = 2))
  }

  @Test
  fun directMessagesComeFirst() {
    val list = ChatListState(
      maybeConversations = listOf(conversation("dm", ConversationKind.DIRECT), conversation("channel", ConversationKind.PUBLIC)),
      previews = mapOf(entry("dm", "1", 1L), entry("channel", "2", 9L)),
    )
    assertEquals(listOf("dm"), FeedSeeds().toReadAhead(list, limit = 1))
  }

  @Test
  fun aLongFeedKeepsOnlyItsNewestMessages() {
    val ids = (1..FEED_SEED_MESSAGES + 5).map { it.toString() }
    val trimmed = seed(ids, hasOlder = false).trimmed()
    assertEquals(ids.takeLast(FEED_SEED_MESSAGES), trimmed.messages.map { it.id })
    assertTrue(trimmed.hasOlder)
  }

  @Test
  fun theLeastRecentlyUsedGoesFirst() {
    val seeds = FeedSeeds(capacity = 2)
    seeds.keep("a", seed(listOf("1")))
    seeds.keep("b", seed(listOf("2")))
    seeds["a"]
    seeds.keep("c", seed(listOf("3")))
    assertNull(seeds["b"])
    assertEquals("1", seeds["a"]?.messages?.single()?.id)
  }

  private fun seed(ids: List<String>, hasOlder: Boolean = false) =
    FeedSeed(ids.map(::message), hasOlder, emptyMap(), FirstUnread.None)

  private fun entry(conversation: String, lastMessageId: String, sentAt: Long) =
    conversation to ChatListEntry(ChatPreview(PreviewSender.None, PreviewContent.Text("text"), sentAt), lastMessageId)

  private fun conversation(id: String, kind: ConversationKind) = ConversationItem(
    id = id,
    maybeSpaceId = null,
    maybeSpaceName = null,
    kind = kind,
    name = id,
    topic = "",
    unreadCount = 0u,
    mentionCount = 0u,
    isFavorite = false,
    isMuted = false,
    canPost = true,
  )

  private fun message(id: String) = Message(id = id, conversation = "c", author = "a", sentAt = id.toLong(), body = "text $id")
}
