package de.zeitgleich.teamcraft.notifications

/**
 * What a conversation's message notification shows and does, worked out from its decrypted lines
 * and [MessagePrivacy]. Pure, so the mapping is unit-tested; [MessageNotifier] only turns it into a
 * `NotificationCompat` notification.
 *
 * One conversation is one notification (tag = conversation ID): a new message, a late decryption
 * or a changed setting replaces it under the same ID, so watch bridges and Wear OS see one
 * notification per conversation, not one per step (issue #739).
 */
data class MessageNotificationSpec(
  val conversationId: String,
  val channel: String,
  /** `EXTRA_TITLE`: what watch bridges that don't read MessagingStyle show as the headline. */
  val title: String,
  /** `EXTRA_TEXT`: the newest message, or "New message from Alice" when only the sender shows. */
  val text: String,
  /**
   * The lock screen's public version (also what watch bridges show while the phone is locked):
   * the sender, never the text; "New message" only when the setting hides names.
   */
  val publicTitle: String,
  val publicText: String,
  /** MessagingStyle, only when the content shows. */
  val maybeConversation: ConversationStyleSpec?,
  /** The sender whose picture is the large icon (one-to-one chat, sender only). */
  val maybeLargeIconSender: NotificationSender?,
  /** Messages from others this notification stands for. */
  val messageCount: Int,
  val whenMs: Long,
  val alerting: NotificationAlerting,
  /** Conversation shortcut and LocusId (the shade's conversation section, bubbles, Wear OS). */
  val maybeShortcut: ShortcutSpec?,
  /** Reply (with remote input), mark as read and mute; empty when the notification may not act. */
  val actions: List<MessageAction>,
  /** The thread a reply goes to: the newest message's. */
  val maybeReplyThreadId: String?,
)

/** MessagingStyle: the conversation's lines with their senders. */
data class ConversationStyleSpec(
  val maybeConversationTitle: String?,
  /** A channel or a chat with several senders: Wear OS and the shade then name each sender. */
  val isGroupConversation: Boolean,
  val messages: List<StyleMessage>,
)

/** One line of the MessagingStyle; `maybeSender == null` is the user's own reply. */
data class StyleMessage(val text: String, val sentAtMs: Long, val maybeSender: NotificationSender?)

/** A sender as a notification `Person`: the key picks the avatar (picture or initials). */
data class NotificationSender(val key: String, val name: String)

data class ShortcutSpec(val id: String, val label: String, val maybePerson: NotificationSender?)

enum class NotificationAlerting {
  /** Sound, vibration and heads-up (once per message). */
  ALERT,

  /** An update of what is shown: replaces it without alerting again (`setOnlyAlertOnce`). */
  SILENT,
}

enum class MessageAction {
  REPLY,
  MARK_READ,
  MUTE,
}

/**
 * The group summary. Android shows it only with two or more conversations; it never alerts itself
 * (`GROUP_ALERT_CHILDREN`), and it carries the names or a count, not a second "New message".
 */
data class SummarySpec(
  val title: String,
  val text: String,
  val lines: List<String>,
  val number: Int,
  /** The lock screen's public version: the conversation names, or a count when names are hidden. */
  val publicTitle: String,
  val publicText: String,
)

/** Builds [MessageNotificationSpec] and [SummarySpec]. */
object MessageNotificationSpecs {
  /** Summary lines shown at most (InboxStyle shows about that many). */
  private const val MAX_SUMMARY_LINES = 6

  /** The conversation's notification, or `null` when it has no message from somebody else. */
  fun conversation(
    entry: ConversationNotification,
    privacy: MessagePrivacy,
    isAlerting: Boolean,
    labels: NotificationText.Labels,
  ): MessageNotificationSpec? {
    val received = entry.lines.filter { !it.isOwn }
    val newest = received.lastOrNull() ?: return null
    val senders = received.mapNotNull { line -> line.maybeSenderKey?.let { NotificationSender(it, line.senderName) } }.distinct()
    val isGroup = entry.maybeTitle != null || senders.map { it.key }.distinct().size > 1
    val newestSender = newest.maybeSenderKey?.let { NotificationSender(it, newest.senderName) }
    val headline = entry.maybeTitle ?: newest.senderName.ifBlank { labels.appName }
    val (publicTitle, publicText) = if (privacy.showsNames) {
      headline to NotificationText.senderOnly(entry.lines, labels)
    } else {
      labels.appName to newMessages(received.size, labels)
    }
    val (title, text) = when {
      privacy.showsContent -> headline to if (isGroup && entry.maybeTitle != null && newest.senderName.isNotBlank()) {
        "${newest.senderName}: ${newest.text}"
      } else {
        newest.text
      }
      else -> publicTitle to publicText
    }
    val conversation = if (privacy.showsContent) {
      ConversationStyleSpec(
        maybeConversationTitle = entry.maybeTitle,
        isGroupConversation = isGroup,
        messages = entry.lines.map { line ->
          StyleMessage(line.text, line.sentAtMs, line.maybeSenderKey?.let { NotificationSender(it, line.senderName) })
        },
      )
    } else {
      null
    }
    val shortcut = if (privacy.pushesShortcut) {
      ShortcutSpec(
        id = entry.conversationId,
        label = headline,
        maybePerson = newestSender.takeIf { entry.maybeTitle == null },
      )
    } else {
      null
    }
    return MessageNotificationSpec(
      conversationId = entry.conversationId,
      channel = when {
        entry.isMention -> NotificationChannels.MENTIONS
        entry.isDirect -> NotificationChannels.DIRECT_MESSAGES
        else -> NotificationChannels.MESSAGES
      },
      title = title,
      text = text,
      publicTitle = publicTitle,
      publicText = publicText,
      maybeConversation = conversation,
      maybeLargeIconSender = newestSender.takeIf { !privacy.showsContent && privacy.showsNames && entry.maybeTitle == null },
      messageCount = received.size,
      whenMs = newest.sentAtMs,
      alerting = if (isAlerting) NotificationAlerting.ALERT else NotificationAlerting.SILENT,
      maybeShortcut = shortcut,
      actions = if (privacy.offersActions) listOf(MessageAction.REPLY, MessageAction.MARK_READ, MessageAction.MUTE) else emptyList(),
      maybeReplyThreadId = newest.maybeThreadId,
    )
  }

  /**
   * The group summary, or `null` while one conversation shows: a lone conversation needs none, and
   * watch bridges mirror a summary as a second notification. `isShown`: a summary is up already;
   * it then stays until no conversation is left (cancelling a summary cancels its children too).
   * `conversationsTitle`: "2 conversations".
   */
  fun summary(
    conversations: Collection<ConversationNotification>,
    privacy: MessagePrivacy,
    maybeBadge: Int?,
    isShown: Boolean,
    labels: NotificationText.Labels,
    conversationsTitle: (Int) -> String,
  ): SummarySpec? {
    if (conversations.isEmpty()) return null
    if (conversations.size < 2 && !isShown) return null
    val messageCount = conversations.sumOf { entry -> entry.lines.count { !it.isOwn } }
    val names = MessagePrivacy.summaryLines(privacy, conversations).filter { it.isNotBlank() }
    val title = conversationsTitle(conversations.size)
    val text = if (names.isEmpty()) newMessages(messageCount, labels) else names.joinToString(", ")
    return SummarySpec(
      title = title,
      text = text,
      lines = names.take(MAX_SUMMARY_LINES),
      number = maybeBadge ?: conversations.size,
      publicTitle = if (names.isEmpty()) labels.appName else title,
      publicText = text,
    )
  }

  private fun newMessages(count: Int, labels: NotificationText.Labels): String =
    if (count <= 1) labels.newMessage else labels.newMessages(count)
}
