package de.zeitgleich.teamcraft

import android.Manifest
import android.app.Notification
import android.app.NotificationManager
import android.os.Build
import android.os.SystemClock
import androidx.core.app.NotificationCompat
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import de.zeitgleich.teamcraft.calls.IncomingCallNotifier
import de.zeitgleich.teamcraft.calls.IncomingRing
import de.zeitgleich.teamcraft.notifications.AvatarCache
import de.zeitgleich.teamcraft.notifications.HoldEnd
import de.zeitgleich.teamcraft.notifications.MessageNotifier
import de.zeitgleich.teamcraft.notifications.NotificationChannels
import de.zeitgleich.teamcraft.notifications.NotificationContentPrefs
import uniffi.teamcraft_ffi.NotificationGap
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
    notifier = notifier()
    // The system drops updates of an app that posts over about 5 a second: tests don't add up.
    SystemClock.sleep(RATE_LIMIT_REST_MS)
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

    eventually {
      val shown = active(CONVERSATION)!!.notification
      assertEquals(NotificationChannels.MESSAGES, shown.channelId)
      val style = NotificationCompat.MessagingStyle.extractMessagingStyleFromNotification(shown)
      assertEquals(listOf("first", "second"), style?.messages?.map { it.text.toString() })
      assertEquals(CONVERSATION, shown.shortcutId)
      // One conversation needs no summary (a watch would mirror it as a second notification).
      assertTrue(system.activeNotifications.none { it.notification.flags and Notification.FLAG_GROUP_SUMMARY != 0 })
    }
    notifier.show(message("3000", text = "elsewhere", conversationId = OTHER_CONVERSATION), isAlerting = false)
    eventually {
      assertTrue(system.activeNotifications.any { it.notification.flags and Notification.FLAG_GROUP_SUMMARY != 0 })
    }
    notifier.dismiss(OTHER_CONVERSATION)

    // Read on another device up to the first message: one line stays.
    SystemClock.sleep(RATE_LIMIT_REST_MS)
    notifier.removeRead(CONVERSATION, maybeThreadId = null, maybeUpToMessageId = "1000", maybeBadge = 1u)
    eventually {
      val updated = NotificationCompat.MessagingStyle.extractMessagingStyleFromNotification(active(CONVERSATION)!!.notification)
      assertEquals(listOf("second"), updated?.messages?.map { it.text.toString() })
    }

    // Read up to the newest: the notification and the summary go.
    notifier.removeRead(CONVERSATION, maybeThreadId = null, maybeUpToMessageId = "2000", maybeBadge = 0u)
    eventually {
      assertNull(active(CONVERSATION))
      assertTrue(system.activeNotifications.none { it.notification.flags and Notification.FLAG_GROUP_SUMMARY != 0 })
    }
  }

  @Test
  fun aRingShowsAnIncomingCallUntilCancelled() {
    val ringer = IncomingCallNotifier(context)
    ringer.show(IncomingRing(CONVERSATION, maybeCallId = "77", displayName = "planning", maybeCallerName = "Alice", isVerified = true))
    eventually {
      val ring = system.activeNotifications.firstOrNull { it.notification.channelId == NotificationChannels.INCOMING_CALLS }
      assertNotNull(ring)
      assertEquals(Notification.CATEGORY_CALL, ring!!.notification.category)
    }
    ringer.cancel()
    eventually {
      assertTrue(system.activeNotifications.none { it.notification.channelId == NotificationChannels.INCOMING_CALLS })
    }
  }

  /** Android Auto's requirements for a messaging notification, as the system holds it (#1250). */
  @Test
  fun aMessageWithItsTextHasWhatAndroidAutoNeeds() {
    notifier.show(message("1000", text = "lunch?"), isAlerting = true)

    eventually {
      val shown = active(CONVERSATION)!!.notification
      assertEquals(Notification.CATEGORY_MESSAGE, shown.category)
      assertFalse(isLocalOnly(shown))
      assertFalse(shown.flags and Notification.FLAG_GROUP_SUMMARY != 0)
      assertFalse(shown.flags and Notification.FLAG_ONLY_ALERT_ONCE != 0)
      assertFalse(isSilent(shown))

      val style = NotificationCompat.MessagingStyle.extractMessagingStyleFromNotification(shown)!!
      assertEquals("lunch?", style.messages.single().text.toString())
      assertEquals("Alice", style.messages.single().person?.name?.toString())
      assertTrue(style.user.name!!.isNotEmpty())

      val actions = (0 until NotificationCompat.getActionCount(shown)).map { NotificationCompat.getAction(shown, it)!! }
      val reply = actions.single { it.semanticAction == NotificationCompat.Action.SEMANTIC_ACTION_REPLY }
      assertEquals(1, reply.remoteInputs?.size)
      assertFalse(reply.showsUserInterface)
      if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) assertFalse(reply.actionIntent!!.isImmutable)
      val markRead = actions.single { it.semanticAction == NotificationCompat.Action.SEMANTIC_ACTION_MARK_AS_READ }
      assertFalse(markRead.showsUserInterface)
    }
  }

  /** Bridges and Android Auto never get "New message"; the phone waits a moment for the text. */
  @Test
  fun aPlaceholderStaysOnThePhoneThenAlertsOnceWithItsText() {
    notifier.show(placeholder("1000"), isAlerting = true, payload = PAYLOAD)
    eventually {
      val held = active(CONVERSATION)!!.notification
      assertTrue(isLocalOnly(held))
      assertTrue(isSilent(held))
    }
    assertEquals(listOf(PAYLOAD.toList()), notifier.placeholderPayloads().map { it.toList() })

    assertTrue(notifier.fillIn(message("1000", text = "lunch?")))
    eventually {
      val filled = active(CONVERSATION)!!.notification
      assertFalse(isLocalOnly(filled))
      assertFalse(isSilent(filled))
      assertFalse(filled.flags and Notification.FLAG_ONLY_ALERT_ONCE != 0)
      assertEquals("lunch?", filled.extras.getCharSequence(Notification.EXTRA_TEXT).toString())
    }
    assertTrue(notifier.placeholderPayloads().isEmpty())
    // Already complete: nothing to post again.
    assertFalse(notifier.fillIn(message("1000", text = "lunch?")))
  }

  /** The hold ended: the phone alerted once; the text then goes to bridges without a second sound. */
  @Test
  fun aPlaceholderThatAlertedIsFilledInSilentlyAndMirrored() {
    notifier.show(placeholder("1000"), isAlerting = true, payload = PAYLOAD)
    assertTrue(notifier.endHold(PAYLOAD, HoldEnd.ALERT))
    assertFalse(notifier.endHold(PAYLOAD, HoldEnd.ALERT))
    eventually {
      val released = active(CONVERSATION)!!.notification
      assertTrue(isLocalOnly(released))
      assertFalse(isSilent(released))
    }

    assertTrue(notifier.fillIn(message("1000", text = "lunch?")))
    eventually {
      val filled = active(CONVERSATION)!!.notification
      assertFalse(isLocalOnly(filled))
      assertTrue(isSilent(filled))
      // Bridges skip updates with this flag.
      assertFalse(filled.flags and Notification.FLAG_ONLY_ALERT_ONCE != 0)
    }
  }

  /** In a car a late text alerts anyway: Android Auto announces what alerts. */
  @Test
  fun inACarALateTextAlerts() {
    val inCar = notifier(isInCar = true)
    inCar.show(placeholder("1000"), isAlerting = true, payload = PAYLOAD)
    assertTrue(inCar.endHold(PAYLOAD, HoldEnd.ALERT))
    assertTrue(inCar.fillIn(message("1000", text = "lunch?")))
    eventually {
      val filled = active(CONVERSATION)!!.notification
      assertFalse(isLocalOnly(filled))
      assertFalse(isSilent(filled))
    }
  }

  /** A second object reads what the first stored; a real process death needs a shell (see the PR). */
  @Test
  fun aPlaceholderAndItsHoldAreReadBackFromTheStore() {
    notifier.show(placeholder("1000"), isAlerting = true, payload = PAYLOAD)
    eventually { assertTrue(isSilent(active(CONVERSATION)!!.notification)) }

    val restarted = notifier()
    assertEquals(listOf(PAYLOAD.toList()), restarted.placeholderPayloads().map { it.toList() })
    assertEquals(listOf(PAYLOAD.toList()), restarted.heldLines().map { it.payload.toList() })
    assertTrue(restarted.endHold(PAYLOAD, HoldEnd.ALERT))
    eventually { assertFalse(isSilent(active(CONVERSATION)!!.notification)) }
    assertTrue(restarted.fillIn(message("1000", text = "lunch?")))
    eventually { assertFalse(isLocalOnly(active(CONVERSATION)!!.notification)) }
  }

  @Test
  fun anUrgentMessageIsNotHeld() {
    notifier.show(placeholder("1000").copy(isUrgent = true), isAlerting = true, payload = PAYLOAD)
    eventually {
      val shown = active(CONVERSATION)!!.notification
      assertFalse(isSilent(shown))
      assertTrue(isLocalOnly(shown))
    }
    assertTrue(notifier.heldLines().isEmpty())
  }

  @Test
  fun aHoldEndsSilentlyOrWithTheLineGone() {
    notifier.show(placeholder("1000"), isAlerting = true, payload = PAYLOAD)
    assertTrue(notifier.endHold(PAYLOAD, HoldEnd.SILENT))
    eventually { assertTrue(isSilent(active(CONVERSATION)!!.notification)) }
    assertTrue(notifier.placeholderPayloads().isNotEmpty())

    notifier.show(placeholder("2000"), isAlerting = true, payload = OTHER_PAYLOAD)
    assertTrue(notifier.endHold(OTHER_PAYLOAD, HoldEnd.DROP))
    assertEquals(listOf(PAYLOAD.toList()), notifier.placeholderPayloads().map { it.toList() })
  }

  /** The message was read on another device before the text came. */
  @Test
  fun aLineReadElsewhereGoesInsteadOfBeingFilledIn() {
    notifier.show(placeholder("1000"), isAlerting = true, payload = PAYLOAD)
    assertTrue(notifier.applyRead(message("1000", text = "lunch?").copy(isRead = true)))
    eventually { assertNull(active(CONVERSATION)) }
    assertFalse(notifier.applyRead(message("1000", text = "lunch?").copy(isRead = true)))
  }

  /** A repeat of a wake-up (a job after its run) doesn't bring back a notification read meanwhile. */
  @Test
  fun aRepeatDoesNotResurrectAReadMessage() {
    assertFalse(notifier.show(message("1000", text = "lunch?").copy(isDuplicate = true), isAlerting = false))
    assertNull(active(CONVERSATION))
  }

  /** A real burst: the system sheds updates over its rate; the text still ends up shown. */
  @Test
  fun aFilledInTextSurvivesABurstOfUpdates() {
    notifier.show(placeholder("1000"), isAlerting = true, payload = PAYLOAD)
    eventually { assertTrue(isLocalOnly(active(CONVERSATION)!!.notification)) }
    repeat(UPDATE_BURST) { notifier.repostAll() }
    assertTrue(notifier.fillIn(message("1000", text = "lunch?")))
    eventually(timeoutMs = 5_000) { assertFalse(isLocalOnly(active(CONVERSATION)!!.notification)) }
  }

  @Test
  fun anAlertAtTheEndOfAHoldSurvivesABurstOfUpdates() {
    notifier.show(placeholder("1000"), isAlerting = true, payload = PAYLOAD)
    eventually { assertTrue(isSilent(active(CONVERSATION)!!.notification)) }
    repeat(UPDATE_BURST) { notifier.repostAll() }
    assertTrue(notifier.endHold(PAYLOAD, HoldEnd.ALERT))
    eventually(timeoutMs = 5_000) { assertFalse(isSilent(active(CONVERSATION)!!.notification)) }
  }

  /** The newest line had its text all along: only the system's revision shows the older one's fill. */
  @Test
  fun anOlderLinesTextSurvivesABurstOfUpdates() {
    notifier.show(placeholder("1000"), isAlerting = true, payload = PAYLOAD)
    notifier.show(message("2000", text = "newer"), isAlerting = false)
    eventually { assertEquals(2, messages().size) }
    repeat(UPDATE_BURST) { notifier.repostAll() }
    assertTrue(notifier.fillIn(message("1000", text = "lunch?")))
    eventually(timeoutMs = 5_000) { assertEquals(listOf("lunch?", "newer"), messages()) }
  }

  @Test
  fun aFilledInMentionMovesToTheMentionsChannel() {
    notifier.show(placeholder("1000"), isAlerting = true, payload = PAYLOAD)
    assertTrue(notifier.fillIn(message("1000", text = "lunch, Bob?").copy(isMention = true)))
    eventually { assertEquals(NotificationChannels.MENTIONS, active(CONVERSATION)!!.notification.channelId) }
  }

  @Test
  fun aPlaceholderThatWasNoMessageGoes() {
    notifier.show(placeholder("1000"), isAlerting = true, payload = PAYLOAD)
    assertTrue(notifier.applyHiddenChange(placeholder("1000")))
    eventually { assertNull(active(CONVERSATION)) }
    // Nothing to update: the caller shows the quiet note instead.
    assertFalse(notifier.applyHiddenChange(placeholder("1000")))
  }

  @Test
  fun aHiddenChangeLeavesTheMessagesShown() {
    notifier.show(message("1000", text = "lunch?"), isAlerting = true)
    assertTrue(notifier.applyHiddenChange(placeholder("2000")))
    eventually {
      val shown = active(CONVERSATION)!!.notification
      val style = NotificationCompat.MessagingStyle.extractMessagingStyleFromNotification(shown)
      assertEquals(listOf("lunch?"), style?.messages?.map { it.text.toString() })
    }
  }

  @Test
  fun theQuietNoteIsNeverMirroredAndGoesByItself() {
    notifier.showQuietNote()
    eventually {
      val note = system.activeNotifications.single { it.tag == "quiet_note" }.notification
      assertTrue(isLocalOnly(note))
      assertTrue(note.timeoutAfter in 1..10_000L)
      assertEquals(NotificationChannels.OTHER, note.channelId)
    }
    eventually(timeoutMs = 10_000) { assertTrue(system.activeNotifications.none { it.tag == "quiet_note" }) }
    // A burst: within the gap nothing goes up again.
    notifier.showQuietNote()
    SystemClock.sleep(500)
    assertTrue(system.activeNotifications.none { it.tag == "quiet_note" })
  }

  @Test
  fun aWakeUpThatDidNotOpenIsPhoneOnlyAndReplacedWhenItOpens() {
    notifier.showUnreadable()
    eventually { assertTrue(isLocalOnly(system.activeNotifications.single { it.tag == "unreadable" }.notification)) }

    notifier.showOpenedLate(message("1000", text = "lunch?"), isAlerting = false, payload = PAYLOAD)
    eventually {
      assertTrue(system.activeNotifications.none { it.tag == "unreadable" })
      val opened = active(CONVERSATION)!!.notification
      assertFalse(isLocalOnly(opened))
      assertTrue(isSilent(opened))
    }
  }

  /** The system applies a post or a cancel on its own thread, a moment after the call returned. */
  private fun eventually(timeoutMs: Long = 3_000, check: () -> Unit) {
    val deadline = SystemClock.uptimeMillis() + timeoutMs
    while (true) {
      try {
        check()
        return
      } catch (failure: Throwable) {
        if (SystemClock.uptimeMillis() > deadline) throw failure
        SystemClock.sleep(POLL_MS)
      }
    }
  }

  private fun notifier(isInCar: Boolean = false) = MessageNotifier(
    context,
    NotificationContentPrefs(context),
    AvatarCache(context) { error("no core in this test") },
    isInCar = { isInCar },
  )

  private fun messages() =
    NotificationCompat.MessagingStyle.extractMessagingStyleFromNotification(active(CONVERSATION)!!.notification)
      ?.messages.orEmpty().map { it.text.toString() }

  private fun isLocalOnly(notification: Notification) = notification.flags and Notification.FLAG_LOCAL_ONLY != 0

  /** What `setSilent` leaves on the notification: a group child that doesn't alert. */
  private fun isSilent(notification: Notification) = notification.groupAlertBehavior == Notification.GROUP_ALERT_SUMMARY

  private fun placeholder(id: String) = message(id, text = "").copy(maybeText = null, maybeGap = NotificationGap.NOT_DECRYPTED)

  private fun active(tag: String) =
    system.activeNotifications.firstOrNull { it.tag == tag && it.id == MessageNotifier.MESSAGE_ID }

  private fun message(id: String, text: String, conversationId: String = CONVERSATION) = PushNotification(
    kind = PushNotificationKind.MESSAGE,
    conversationId = conversationId,
    maybeMessageId = id,
    maybeConversationName = "planning",
    maybeSenderId = "300",
    maybeSenderName = "Alice",
    maybeText = text,
    maybeFileName = null,
    maybeAttachmentKind = null,
    maybeGap = null,
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
    isRead = false,
    maybeSuppression = null,
  )

  private companion object {
    const val CONVERSATION = "4242"
    const val OTHER_CONVERSATION = "4343"
    const val POLL_MS = 50L
    const val RATE_LIMIT_REST_MS = 1_200L
    const val UPDATE_BURST = 20
    val PAYLOAD = byteArrayOf(1, 2, 3)
    val OTHER_PAYLOAD = byteArrayOf(4, 5, 6)
  }
}
