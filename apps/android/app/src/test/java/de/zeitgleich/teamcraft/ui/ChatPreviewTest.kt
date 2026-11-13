package de.zeitgleich.teamcraft.ui

import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Test
import uniffi.teamcraft_ffi.ConversationItem
import uniffi.teamcraft_ffi.ConversationKind
import de.zeitgleich.teamcraft.core.ChatPreviews
import de.zeitgleich.teamcraft.core.Message
import de.zeitgleich.teamcraft.core.WireJson

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

  /** Issue #1876: the lines come as the feed's messages with only their authors, named as before. */
  @Test
  fun previewsNameTheirAuthorsFromTheRead() {
    val json = """
      {"messages": [
        {"id": "1", "conversation": "a", "author": "2", "kind": "User", "sent_at": 1, "body": "Hi"},
        {"id": "2", "conversation": "b", "author": "3", "kind": "User", "sent_at": 2, "body": "Bye"},
        {"id": "3", "conversation": "c", "author": "8", "kind": {"Log": "Info"}, "sent_at": 3, "body": "green",
          "source": {"name": "CI"}},
        {"id": "4", "conversation": "d", "author": "7", "kind": "User", "sent_at": 4, "body": "old",
          "imported": {"source": "synology_chat", "author": {"kind": "named", "name": "Old Bot"}, "importer": "6"}},
        {"id": "5", "conversation": "e", "author": "7", "kind": "User", "sent_at": 5, "body": "older",
          "imported": {"source": "synology_chat", "author": {"kind": "deleted"}, "importer": "6"}},
        {"id": "6", "conversation": "f", "author": "9", "kind": "User", "sent_at": 6, "body": "", "placeholder": "waiting"}
      ], "authors": [
        {"id": "2", "username": "sam", "display_name": "Sam Doe"},
        {"id": "3", "username": "", "display_name": "Tombstone", "is_deleted": true}
      ]}
    """
    val read = WireJson.decodeFromString(ChatPreviews.serializer(), json)
    val authors = read.authors.associateBy { it.id }
    val names = read.messages.map { it.authorName(authors, "Deleted user") }
    assertEquals(listOf("Sam Doe", "Deleted user", "CI", "Old Bot", "Deleted user", ""), names)
    val waiting = chatPreview(read.messages.last(), ME, names.last(), isOneToOne = false)
    assertEquals(PreviewContent.Placeholder("waiting"), waiting.content)
  }

  @Test
  fun directMessagesAreSortedByRecency() {
    val result = directSections(
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
