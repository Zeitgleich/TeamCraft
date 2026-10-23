package de.zeitgleich.teamcraft.ui

import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Test
import uniffi.teamcraft_ffi.ConversationItem
import uniffi.teamcraft_ffi.ConversationKind
import de.zeitgleich.teamcraft.core.Message

class ChatPreviewTest {
  @Test
  fun channelsNameTheSenderDirectMessagesOnlyMe() {
    assertEquals(PreviewSender.Other("Alice"), chatPreview(message("alice", "Hi"), ME, "Alice", isOneToOne = false).sender)
    assertEquals(PreviewSender.None, chatPreview(message("alice", "Hi"), ME, "Alice", isOneToOne = true).sender)
    assertEquals(PreviewSender.Me, chatPreview(message(ME, "Hi"), ME, "Bob", isOneToOne = true).sender)
  }

  @Test
  fun textIsFoldedIntoOneShortLine() {
    val preview = chatPreview(message("alice", "  Line one\n\nline   two  "), ME, "Alice", isOneToOne = false)
    assertEquals(PreviewContent.Text("Line one line two"), preview.content)
    assertEquals(160, oneLine("x".repeat(500)).length)
  }

  @Test
  fun emptyBodiesAndEventsHaveTheirOwnContent() {
    assertEquals(PreviewContent.Deleted, chatPreview(message("alice", ""), ME, "Alice", isOneToOne = false).content)
    val event = message("alice", "").copy(kind = JsonObject(mapOf("System" to JsonPrimitive("Joined"))))
    val preview = chatPreview(event, ME, "Alice", isOneToOne = false)
    assertEquals(PreviewContent.Event, preview.content)
    assertEquals(PreviewSender.None, preview.sender)
  }

  @Test
  fun directMessagesAreSortedByRecency() {
    val result = sections(
      listOf(item("1", "Alice"), item("2", "Bob"), item("3", "Carol")),
      lastActivity = mapOf("3" to 30L, "1" to 10L),
    )
    assertEquals(listOf("Carol", "Alice", "Bob"), result.single().conversations.map { it.name })
  }

  private fun message(author: String, body: String) =
    Message(id = "m", conversation = "c", author = author, sentAt = 1L, body = body)

  private fun item(id: String, name: String) = ConversationItem(
    id = id,
    maybeSpaceId = null,
    maybeSpaceName = null,
    kind = ConversationKind.DIRECT,
    name = name,
    topic = "",
    unreadCount = 0u,
    mentionCount = 0u,
    isFavorite = false,
    isMuted = false,
    canPost = true,
  )

  private companion object {
    const val ME = "me"
  }
}
