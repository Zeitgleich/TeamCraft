package de.zeitgleich.teamcraft.notifications

/**
 * Title and lines of a message notification, from what the push preview could read. Pure, so the
 * choices are unit-tested; the localized fallbacks come in as parameters.
 */
data class NotificationText(
  /** The conversation's name for channels and group DMs; `null` for one-to-one DMs. */
  val maybeConversationTitle: String?,
  val senderName: String,
  val text: String,
) {
  companion object {
    fun from(
      maybeConversationName: String?,
      maybeSenderName: String?,
      maybeText: String?,
      maybeFileName: String?,
      isDirect: Boolean,
      fallbackSender: String,
      fallbackText: String,
      fileText: (String) -> String,
    ): NotificationText {
      val sender = maybeSenderName?.takeIf { it.isNotBlank() } ?: maybeConversationName ?: fallbackSender
      val text = when {
        !maybeText.isNullOrBlank() -> maybeText
        !maybeFileName.isNullOrBlank() -> fileText(maybeFileName)
        else -> fallbackText
      }
      val title = maybeConversationName?.takeIf { !isDirect && it.isNotBlank() }
      return NotificationText(title, sender, text)
    }
  }
}
