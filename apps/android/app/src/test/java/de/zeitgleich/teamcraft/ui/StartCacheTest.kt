package de.zeitgleich.teamcraft.ui

import kotlinx.serialization.json.Json
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import uniffi.teamcraft_ffi.ConversationItem
import uniffi.teamcraft_ffi.ConversationKind
import uniffi.teamcraft_ffi.Presence

class StartCacheTest {
  @Test
  fun theListComesBackAsItWasKept() {
    val list = ChatListState(
      maybeConversations = listOf(CHANNEL, DIRECT),
      previews = mapOf(
        CHANNEL.id to ChatListEntry(ChatPreview(PreviewSender.Other("Bob"), PreviewContent.Text("Merged"), 1_000L), "11"),
        DIRECT.id to ChatListEntry(ChatPreview(PreviewSender.Me, PreviewContent.Placeholder("waiting"), 2_000L), "12"),
      ),
      runningCalls = mapOf(CHANNEL.id to 2),
    )
    val cache = StartCache.of("https://example.com subject", READY, list)!!
    val restored = Json.decodeFromString<StartCache>(Json.encodeToString(cache))
    assertEquals(READY, restored.ready())
    // Running calls are never shown from a cache: they may have ended.
    assertEquals(list.copy(runningCalls = emptyMap()), restored.listState())
  }

  @Test
  fun nothingIsKeptBeforeTheListWasRead() {
    assertNull(StartCache.of("https://example.com subject", READY, ChatListState()))
  }

  @Test
  fun aKindOfANewerVersionIsLeftOut() {
    val cached = CachedConversation.of(CHANNEL).copy(kind = "SOMETHING_NEW")
    assertNull(cached.item())
  }

  @Test
  fun everyPreviewShapeSurvives() {
    val entries = listOf(
      ChatPreview(PreviewSender.None, PreviewContent.Event, 1L),
      ChatPreview(PreviewSender.Other("Carol"), PreviewContent.Deleted, 2L),
      ChatPreview(PreviewSender.Me, PreviewContent.Text("Hi"), 3L),
    ).map { ChatListEntry(it, "1") }
    for (entry in entries) assertEquals(entry, CachedEntry.of(entry).entry())
  }

  private companion object {
    val READY = AppState.Ready("1", "Carol", "fingerprint")
    val CHANNEL = ConversationItem(
      id = "100",
      maybeSpaceId = "7",
      maybeSpaceName = "Project A",
      kind = ConversationKind.PRIVATE,
      name = "general",
      topic = "",
      unreadCount = 3u,
      mentionCount = 1u,
      isFavorite = true,
      isMuted = false,
      canPost = true,
    )
    val DIRECT = ConversationItem(
      id = "200",
      maybeSpaceId = null,
      maybeSpaceName = null,
      kind = ConversationKind.DIRECT,
      name = "Alice",
      topic = "",
      unreadCount = 0u,
      mentionCount = 0u,
      isFavorite = false,
      isMuted = true,
      canPost = true,
      maybePeerPresence = Presence.AWAY,
      maybePeerId = "2",
      maybeDraftPreview = "draft",
    )
  }
}
