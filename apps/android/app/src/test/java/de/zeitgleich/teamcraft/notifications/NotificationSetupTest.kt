package de.zeitgleich.teamcraft.notifications

import org.junit.Assert.assertEquals
import org.junit.Test

class NotificationSetupTest {
  @Test
  fun channelsAndAdjustedConversationsAreListedWithoutNames() {
    val text = NotificationSetup.format(
      installer = "com.example.store",
      channels = listOf(
        NotificationSetup.ChannelLine("mentions", 4, hasDefaultSound = true, hasSound = true, vibrates = true),
        NotificationSetup.ChannelLine(
          id = "conversation-channel",
          importance = 3,
          hasDefaultSound = false,
          hasSound = true,
          vibrates = false,
          maybeParentId = "channel_messages",
          maybeConversationId = "1234",
          isImportantConversation = true,
          hasUserImportance = true,
        ),
        NotificationSetup.ChannelLine("channel_messages", 2, hasDefaultSound = false, hasSound = false, vibrates = false),
      ),
    )
    assertEquals(
      """
      Installed from: com.example.store
      Notification channels:
        channel_messages: low (silent), sound none, vibration off
        channel_messages / conversation 1234: default (sound, no pop-up) (set by user), sound custom, vibration off, priority conversation
        mentions: high (sound and pop-up), sound default, vibration on

      """.trimIndent(),
      text,
    )
  }
}
