package de.zeitgleich.teamcraft

import android.Manifest
import android.app.Notification
import android.app.NotificationManager
import android.os.Build
import androidx.core.app.NotificationCompat
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import de.zeitgleich.teamcraft.calls.IncomingCallNotifier
import de.zeitgleich.teamcraft.calls.IncomingRing
import de.zeitgleich.teamcraft.notifications.MessageNotifier
import de.zeitgleich.teamcraft.notifications.NotificationChannels
import uniffi.teamcraft_ffi.PushNotification
import uniffi.teamcraft_ffi.PushNotificationKind

/**
 * The notification channels and the notifications themselves on a real system: what
 * NotificationManager shows after posting, updating and removing.
 */
@RunWith(AndroidJUnit4::class)
class NotificationsOnDeviceTest {
  private val context = InstrumentationRegistry.getInstrumentation().targetContext
  private val system = context.getSystemService(NotificationManager::class.java)
  private lateinit var notifier: MessageNotifier

  @Before
  fun allowNotifications() {
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
      InstrumentationRegistry.getInstrumentation().uiAutomation
        .grantRuntimePermission(context.packageName, Manifest.permission.POST_NOTIFICATIONS)
    }
    notifier = MessageNotifier(context)
    notifier.clearAll()
  }

  @After
  fun removeNotifications() {
    notifier.clearAll()
  }

  @Test
  fun theChannelsExistWithTheirImportance() {
    NotificationChannels.create(context)
    for (channel in NotificationChannels.all) {
      val created = system.getNotificationChannel(channel.id)
      assertNotNull(channel.id, created)
      assertEquals(channel.id, channel.importance, created.importance)
    }
    assertNotNull(system.getNotificationChannel(NotificationChannels.INCOMING_CALLS).sound)
  }

  @Test
  fun aMessageIsPostedAsAConversationAndRemovedWhenReadElsewhere() {
    notifier.show(message("1000", text = "first"), isAlerting = false)
    notifier.show(message("2000", text = "second"), isAlerting = false)

    val posted = active(CONVERSATION)
    assertNotNull(posted)
    val shown = posted!!.notification
    assertEquals(NotificationChannels.MESSAGES, shown.channelId)
    val style = NotificationCompat.MessagingStyle.extractMessagingStyleFromNotification(shown)
    assertEquals(listOf("first", "second"), style?.messages?.map { it.text.toString() })
    assertEquals(CONVERSATION, shown.shortcutId)
    assertTrue(system.activeNotifications.any { it.notification.flags and Notification.FLAG_GROUP_SUMMARY != 0 })

    // Read on another device up to the first message: one line stays.
    notifier.removeRead(CONVERSATION, maybeThreadId = null, maybeUpToMessageId = "1000", maybeBadge = 1u)
    val updated = NotificationCompat.MessagingStyle.extractMessagingStyleFromNotification(active(CONVERSATION)!!.notification)
    assertEquals(listOf("second"), updated?.messages?.map { it.text.toString() })

    // Read up to the newest: the notification and the summary go.
    notifier.removeRead(CONVERSATION, maybeThreadId = null, maybeUpToMessageId = "2000", maybeBadge = 0u)
    assertNull(active(CONVERSATION))
    assertTrue(system.activeNotifications.none { it.notification.flags and Notification.FLAG_GROUP_SUMMARY != 0 })
  }

  @Test
  fun aRingShowsAnIncomingCallUntilCancelled() {
    val ringer = IncomingCallNotifier(context)
    ringer.show(IncomingRing(CONVERSATION, maybeCallId = "77", displayName = "planning", maybeCallerName = "Alice"))
    val ring = system.activeNotifications.firstOrNull { it.notification.channelId == NotificationChannels.INCOMING_CALLS }
    assertNotNull(ring)
    assertEquals(Notification.CATEGORY_CALL, ring!!.notification.category)
    ringer.cancel()
    assertTrue(system.activeNotifications.none { it.notification.channelId == NotificationChannels.INCOMING_CALLS })
  }

  private fun active(tag: String) =
    system.activeNotifications.firstOrNull { it.tag == tag && it.id == MessageNotifier.MESSAGE_ID }

  private fun message(id: String, text: String) = PushNotification(
    kind = PushNotificationKind.MESSAGE,
    conversationId = CONVERSATION,
    maybeMessageId = id,
    maybeConversationName = "planning",
    maybeSenderId = "300",
    maybeSenderName = "Alice",
    maybeText = text,
    maybeFileName = null,
    isDirect = false,
    isMention = false,
    isUrgent = false,
    isCall = false,
    isSilent = false,
    isConversationVerified = true,
    maybeThreadId = null,
    maybeCallId = null,
    maybeBadge = 1u,
    maybeSentAtMs = null,
    isDuplicate = false,
    maybeSuppression = null,
  )

  private companion object {
    const val CONVERSATION = "4242"
  }
}
