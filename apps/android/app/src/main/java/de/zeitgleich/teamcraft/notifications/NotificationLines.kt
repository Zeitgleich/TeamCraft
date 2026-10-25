package de.zeitgleich.teamcraft.notifications

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

/**
 * What a conversation's notification shows, kept across process restarts so a new message adds to
 * the conversation instead of replacing it. Only what the notification itself shows is kept.
 */
@Serializable
data class ConversationNotification(
  @SerialName("conversation_id") val conversationId: String,
  /** The conversation's name for channels and group DMs; `null` for one-to-one DMs. */
  @SerialName("title") val maybeTitle: String? = null,
  /** Offer reply and mark read (the core verified the conversation of the newest message). */
  @SerialName("is_verified") val isVerified: Boolean = false,
  /** The newest message was a mention or urgent: it goes to the mentions channel. */
  @SerialName("is_mention") val isMention: Boolean = false,
  /** A one-to-one or group DM: its messages go to the direct messages channel. */
  @SerialName("is_direct") val isDirect: Boolean = false,
  val lines: List<NotificationLine> = emptyList(),
)

/** One message (or the user's own reply) in a conversation's notification. */
@Serializable
data class NotificationLine(
  /** `null` for the user's own replies from the notification. */
  @SerialName("message_id") val maybeMessageId: String? = null,
  @SerialName("thread_id") val maybeThreadId: String? = null,
  /** `null` for the user's own replies. */
  @SerialName("sender_key") val maybeSenderKey: String? = null,
  @SerialName("sender_name") val senderName: String = "",
  val text: String,
  @SerialName("sent_at_ms") val sentAtMs: Long,
) {
  val isOwn: Boolean get() = maybeSenderKey == null
}

/**
 * Merging and removing notification lines. Pure, so ordering, duplicates and the matching of
 * "read elsewhere" dismissals are unit-tested.
 */
object NotificationLines {
  /** Lines a conversation's notification keeps (MessagingStyle shows the newest few anyway). */
  const val MAX_LINES = 8

  /** Custom epoch of the server's Snowflake IDs (teamcraft-proto `SNOWFLAKE_EPOCH_MS`). */
  private const val SNOWFLAKE_EPOCH_MS = 1_767_225_600_000L
  /** Worker and sequence bits below the timestamp of a Snowflake ID. */
  private const val SNOWFLAKE_LOW_BITS = 22

  /** By send time; same millisecond by ID (Snowflakes sort by time), own replies after. */
  private val LINE_ORDER = compareBy<NotificationLine>({ it.sentAtMs }, { it.maybeMessageId?.toLongOrNull() ?: Long.MAX_VALUE })

  /**
   * Adds `line`, replacing a line of the same message (an urgent repeat or an edit), ordered by the
   * time the message was sent, not by arrival; keeps the newest [MAX_LINES].
   */
  fun merge(lines: List<NotificationLine>, line: NotificationLine): List<NotificationLine> {
    val others = line.maybeMessageId?.let { id -> lines.filterNot { it.maybeMessageId == id } } ?: lines
    return (others + line).sortedWith(LINE_ORDER).takeLast(MAX_LINES)
  }

  /**
   * The lines left after the conversation was read on another device up to `maybeUpToMessageId`
   * (everything when `null`). A thread read removes only that thread's lines; reading the
   * conversation removes its lines outside threads. The user's own replies go once no message of
   * somebody else is left.
   */
  fun afterRead(
    lines: List<NotificationLine>,
    maybeThreadId: String?,
    maybeUpToMessageId: String?,
  ): List<NotificationLine> {
    val upTo = maybeUpToMessageId?.toLongOrNull()
    val left = lines.filterNot { line ->
      !line.isOwn && line.maybeThreadId == maybeThreadId && isUpTo(line, upTo)
    }
    return if (left.any { !it.isOwn }) left else emptyList()
  }

  /** When a message was sent, from its Snowflake ID; `null` if `id` is not one. */
  fun sentAtMs(id: String): Long? {
    val raw = id.toLongOrNull()?.takeIf { it > 0 } ?: return null
    return (raw ushr SNOWFLAKE_LOW_BITS) + SNOWFLAKE_EPOCH_MS
  }

  private fun isUpTo(line: NotificationLine, maybeUpTo: Long?): Boolean {
    val upTo = maybeUpTo ?: return true
    // A line without a usable ID can't be newer than what was read as far as we know.
    val id = line.maybeMessageId?.toLongOrNull() ?: return true
    return id <= upTo
  }
}
