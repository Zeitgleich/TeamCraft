package de.zeitgleich.teamcraft.notifications

import uniffi.teamcraft_ffi.NotificationAttachmentKind

/**
 * Title and lines of a message notification, from what the push preview could read. Pure, so the
 * choices are unit-tested; the localized texts come in as [Labels].
 */
data class NotificationText(
  /** The conversation's name for channels and group DMs; `null` for one-to-one DMs. */
  val maybeConversationTitle: String?,
  val senderName: String,
  val text: String,
) {
  /** The localized texts a notification falls back to. */
  data class Labels(
    val appName: String,
    val newMessage: String,
    /** "New message from Alice". */
    val newMessageFrom: (String) -> String,
    /** "3 new messages from Alice, Bob" (count, names). */
    val newMessagesFrom: (Int, String) -> String,
    /** "Photo", "notes.pdf", … by kind and file name. */
    val attachment: (NotificationAttachmentKind, String) -> String,
  )

  companion object {
    fun from(
      maybeConversationName: String?,
      maybeSenderName: String?,
      maybeText: String?,
      maybeFileName: String?,
      maybeAttachmentKind: NotificationAttachmentKind?,
      isDirect: Boolean,
      labels: Labels,
    ): NotificationText {
      val sender = maybeSenderName?.takeIf { it.isNotBlank() } ?: maybeConversationName ?: labels.appName
      val text = when {
        !maybeText.isNullOrBlank() -> maybeText
        !maybeFileName.isNullOrBlank() ->
          labels.attachment(maybeAttachmentKind ?: NotificationAttachmentKind.FILE, maybeFileName)
        else -> labels.newMessage
      }
      val title = maybeConversationName?.takeIf { !isDirect && it.isNotBlank() }
      return NotificationText(title, sender, text)
    }

    /**
     * The text of a notification that names the senders but not what they wrote: "New message from
     * Alice", or "3 new messages from Alice, Bob" for several (newest sender first).
     */
    fun senderOnly(lines: List<NotificationLine>, labels: Labels): String {
      val received = lines.filter { !it.isOwn }
      val senders = received.asReversed().map { it.senderName }.filter { it.isNotBlank() }.distinct()
      return when {
        senders.isEmpty() -> labels.newMessage
        received.size == 1 -> labels.newMessageFrom(senders.first())
        else -> labels.newMessagesFrom(received.size, senders.joinToString(", "))
      }
    }
  }
}
