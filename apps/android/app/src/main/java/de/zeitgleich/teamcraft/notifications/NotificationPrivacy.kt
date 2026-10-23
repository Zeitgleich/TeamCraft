package de.zeitgleich.teamcraft.notifications

/**
 * What a message notification may show. With the app lock on, the chats open only after the
 * device's biometrics or screen lock, so the notifications must not show what the locked app hides:
 * neither who wrote nor what, also not in the shade of an unlocked phone and not as a launcher
 * shortcut (as on iOS). The lines are still remembered (sealed), so the notification shows them
 * again once the lock is turned off. Calls, test pushes, withdrawals and the badge are not affected.
 */
data class MessagePrivacy(
  /** Sender, conversation name and text appear; otherwise only "New message". */
  val showsContent: Boolean,
  /** Reply and mark read: they would act on a conversation the notification doesn't name. */
  val offersActions: Boolean,
  /** The conversation shortcut, which carries the conversation's or sender's name. */
  val pushesShortcut: Boolean,
) {
  companion object {
    fun of(isAppLockOn: Boolean, isVerified: Boolean): MessagePrivacy =
      if (isAppLockOn) {
        MessagePrivacy(showsContent = false, offersActions = false, pushesShortcut = false)
      } else {
        MessagePrivacy(showsContent = true, offersActions = isVerified, pushesShortcut = true)
      }

    /** The summary's lines: conversation or sender names, none while the app lock is on. */
    fun summaryLines(isAppLockOn: Boolean, conversations: Collection<ConversationNotification>): List<String> =
      if (isAppLockOn) {
        emptyList()
      } else {
        conversations.map { entry -> entry.maybeTitle ?: entry.lines.lastOrNull { !it.isOwn }?.senderName.orEmpty() }
      }
  }
}
