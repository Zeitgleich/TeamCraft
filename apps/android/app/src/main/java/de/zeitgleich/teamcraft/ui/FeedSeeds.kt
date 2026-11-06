package de.zeitgleich.teamcraft.ui

import de.zeitgleich.teamcraft.core.Message
import de.zeitgleich.teamcraft.core.MessageExtras
import uniffi.teamcraft_ffi.ConversationKind

/** How many conversations keep what they showed; a few hundred messages each at most. */
const val FEED_SEED_CAPACITY = 24

/** How many of the newest conversations of the chat list are read ahead. */
const val READ_AHEAD_CONVERSATIONS = 10

/** How many messages a read-ahead or a kept feed holds: a screen and a half on a tall phone. */
const val FEED_SEED_MESSAGES = 40

/**
 * What a conversation shows when it opens, before its first read: what it showed when it was open
 * last, or what was read ahead while the list was shown. The first read replaces it.
 */
data class FeedSeed(
  val messages: List<Message>,
  val hasOlder: Boolean,
  val extras: Map<String, MessageExtras>,
  val firstUnread: FirstUnread,
) {
  /** The newest [FEED_SEED_MESSAGES]: a feed paged far back doesn't keep all of it here. */
  fun trimmed(): FeedSeed {
    if (messages.size <= FEED_SEED_MESSAGES) return this
    val kept = messages.takeLast(FEED_SEED_MESSAGES)
    val ids = kept.mapTo(HashSet()) { it.id }
    return copy(messages = kept, hasOlder = true, extras = extras.filterKeys { it in ids })
  }
}

/** The kept [FeedSeed]s, least recently used out first. Thread-safe. */
class FeedSeeds(private val capacity: Int = FEED_SEED_CAPACITY) {
  private val seeds = object : LinkedHashMap<String, FeedSeed>(capacity, LOAD_FACTOR, true) {
    override fun removeEldestEntry(eldest: MutableMap.MutableEntry<String, FeedSeed>?): Boolean = size > capacity
  }

  @Synchronized
  operator fun get(conversationId: String): FeedSeed? = seeds[conversationId]

  @Synchronized
  fun keep(conversationId: String, seed: FeedSeed) {
    seeds[conversationId] = seed.trimmed()
  }

  @Synchronized
  fun clear() = seeds.clear()

  /**
   * The conversations of [list] to read ahead: the [limit] with the newest messages, direct messages
   * first (the list the app opens on), unless what is kept of them already holds that message (the
   * preview's, which may be followed by a system line).
   */
  @Synchronized
  fun toReadAhead(list: ChatListState, limit: Int = READ_AHEAD_CONVERSATIONS): List<String> {
    val direct = list.maybeConversations.orEmpty()
      .filter { it.kind == ConversationKind.DIRECT || it.kind == ConversationKind.GROUP }
      .mapTo(HashSet()) { it.id }
    return list.previews.entries
      .sortedWith(compareBy<Map.Entry<String, ChatListEntry>> { it.key !in direct }.thenByDescending { it.value.preview.sentAt })
      .take(limit)
      .filter { (id, entry) -> seeds[id]?.messages?.any { it.id == entry.lastMessageId } != true }
      .map { (id, _) -> id }
  }

  private companion object {
    const val LOAD_FACTOR = 0.75f
  }
}
