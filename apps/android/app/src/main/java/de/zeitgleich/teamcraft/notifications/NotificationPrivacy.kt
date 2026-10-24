package de.zeitgleich.teamcraft.notifications

import uniffi.teamcraft_ffi.NotificationContent

/**
 * What a message notification may show: the device's setting ([NotificationContentPrefs]), capped
 * at the sender while the app lock is on. With the app lock on the chats open only after the
 * device's biometrics or screen lock, so the notification names who wrote but not what, and offers
 * no actions or launcher shortcut that would reach into the locked app. The lines are still
 * remembered (sealed), so the notification shows them again once the lock or setting allows it.
 * Calls, test pushes, withdrawals and the badge are not affected.
 */
data class MessagePrivacy(
  /** How much of the conversation the notification shows. */
  val content: NotificationContent,
  /** Reply and mark read: only for a verified conversation the notification names. */
  val offersActions: Boolean,
  /** The conversation shortcut, which carries the conversation's or sender's name. */
  val pushesShortcut: Boolean,
) {
  val showsContent: Boolean get() = content == NotificationContent.CONTENT
  val showsNames: Boolean get() = content != NotificationContent.NOTHING

  companion object {
    fun of(device: NotificationContent, isAppLockOn: Boolean, isVerified: Boolean): MessagePrivacy {
      val content = if (isAppLockOn && device == NotificationContent.CONTENT) NotificationContent.SENDER_ONLY else device
      val showsNames = content != NotificationContent.NOTHING
      return MessagePrivacy(
        content = content,
        offersActions = showsNames && isVerified && !isAppLockOn,
        pushesShortcut = showsNames && !isAppLockOn,
      )
    }

    /** The summary's lines: conversation or sender names, none when names are hidden. */
    fun summaryLines(privacy: MessagePrivacy, conversations: Collection<ConversationNotification>): List<String> =
      if (!privacy.showsNames) {
        emptyList()
      } else {
        conversations.map { entry -> entry.maybeTitle ?: entry.lines.lastOrNull { !it.isOwn }?.senderName.orEmpty() }
      }
  }
}
