package de.zeitgleich.teamcraft.notifications

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Test
import uniffi.teamcraft_ffi.NotificationGap
import uniffi.teamcraft_ffi.NotificationSuppression
import uniffi.teamcraft_ffi.PushNotification
import uniffi.teamcraft_ffi.PushNotificationKind

class PushDecisionTest {
  @Test
  fun aNewMessageAlerts() {
    val wakeUp = push(PushNotificationKind.MESSAGE)
    assertEquals(PushDecision.ShowMessage(wakeUp, isAlerting = true), PushDecision.of(wakeUp))
  }

  /** Issue #756: a conversation that already shows a notification still gets every new message. */
  @Test
  fun everyNewMessageOfABurstIsShown() {
    for (id in listOf("200", "201", "202", "203")) {
      val wakeUp = push(PushNotificationKind.MESSAGE, maybeMessageId = id)
      assertEquals(PushDecision.ShowMessage(wakeUp, isAlerting = true), PushDecision.of(wakeUp))
    }
  }

  /** A reaction or an edit: the server wakes the device for it, but it is no "New message" (#1249). */
  @Test
  fun aHiddenChangeAnnouncesNothing() {
    val wakeUp = push(PushNotificationKind.MESSAGE).copy(maybeGap = NotificationGap.HIDDEN)
    assertEquals(PushDecision.HiddenChange(wakeUp), PushDecision.of(wakeUp))
  }

  /** Being added is the one change its subject hears of: announced, and never a placeholder to retry. */
  @Test
  fun beingAddedIsAnnounced() {
    val wakeUp = push(PushNotificationKind.MESSAGE).copy(maybeGap = NotificationGap.ADDED_YOU, maybeText = null)
    assertEquals(PushDecision.ShowMessage(wakeUp, isAlerting = true), PushDecision.of(wakeUp))
    assertFalse(PushRetry.isPlaceholder(wakeUp))
  }

  @Test
  fun aMessageReadBeforeItOpenedAnnouncesNothing() {
    val wakeUp = push(PushNotificationKind.MESSAGE).copy(isRead = true)
    assertEquals(PushDecision.AlreadyRead(wakeUp), PushDecision.of(wakeUp))
    // Urgent repeats alert until they are acknowledged.
    val urgent = push(PushNotificationKind.MESSAGE, isUrgent = true).copy(isRead = true)
    assertEquals(PushDecision.ShowMessage(urgent, isAlerting = true), PushDecision.of(urgent))
  }

  /** A ring opened late is over: a note that somebody called, never the phone ringing. */
  @Test
  fun aRingOpenedLateDoesNotRing() {
    val ring = push(PushNotificationKind.CALL, maybeCallId = "77")
    assertEquals(PushDecision.ShowQuietCall(ring), PushDecision.of(ring, isRingOver = true))
    assertEquals(PushDecision.Ring(ring.conversationId, "77", ring), PushDecision.of(ring))
    assertEquals(false, PushRetry.isRingOver(null, 1_000_000))
    assertEquals(false, PushRetry.isRingOver(1_000_000, 1_045_000))
    assertEquals(true, PushRetry.isRingOver(1_000_000, 1_045_001))
    // A clock behind the server's: not over, as a first delivery would be too.
    assertEquals(false, PushRetry.isRingOver(1_000_000, 900_000))
  }

  /**
   * Issue #1257: a ring opened late that was answered on another device is no missed call. Only the
   * server's answer says so; without it the late ring stays a missed call.
   */
  @Test
  fun aRingOpenedLateAndAnsweredElsewhereIsNoMissedCall() {
    val answered = push(PushNotificationKind.CALL, maybeCallId = "77").copy(isRingAnswered = true)
    assertEquals(PushDecision.RingAnswered("77"), PushDecision.of(answered, isRingOver = true))
    // Also when the device's own check would have kept it quiet, or the ring isn't over yet.
    assertEquals(PushDecision.RingAnswered("77"), PushDecision.of(answered.copy(maybeSuppression = NotificationSuppression.MUTED)))
    assertEquals(PushDecision.RingAnswered("77"), PushDecision.of(answered))
    val missed = push(PushNotificationKind.CALL, maybeCallId = "77")
    assertEquals(PushDecision.ShowQuietCall(missed), PushDecision.of(missed, isRingOver = true))
  }

  @Test
  fun aSilentMessageShowsWithoutAlerting() {
    val wakeUp = push(PushNotificationKind.MESSAGE, isSilent = true)
    assertEquals(PushDecision.ShowMessage(wakeUp, isAlerting = false), PushDecision.of(wakeUp))
  }

  @Test
  fun aDuplicateUpdatesSilentlySoEveryHighPriorityPushShowsSomething() {
    val wakeUp = push(PushNotificationKind.MESSAGE, isDuplicate = true)
    assertEquals(PushDecision.ShowMessage(wakeUp, isAlerting = false), PushDecision.of(wakeUp))
  }

  @Test
  fun anUrgentRepeatAlertsAgain() {
    // The core never marks urgent repeats as duplicates.
    val wakeUp = push(PushNotificationKind.MESSAGE, isUrgent = true)
    assertEquals(PushDecision.ShowMessage(wakeUp, isAlerting = true), PushDecision.of(wakeUp))
  }

  @Test
  fun theDevicesOwnMuteOrDndCheckShowsNothing() {
    for (suppression in NotificationSuppression.entries) {
      val wakeUp = push(PushNotificationKind.MESSAGE, maybeSuppression = suppression, isDuplicate = true)
      assertEquals(PushDecision.Ignore(IgnoreReason.SUPPRESSED), PushDecision.of(wakeUp))
    }
  }

  @Test
  fun aCallRings() {
    val wakeUp = push(PushNotificationKind.CALL, maybeCallId = "77")
    assertEquals(PushDecision.Ring(CONVERSATION, "77", wakeUp), PushDecision.of(wakeUp))
  }

  @Test
  fun aSuppressedCallStillPostsQuietly() {
    val wakeUp = push(PushNotificationKind.CALL, maybeCallId = "77", maybeSuppression = NotificationSuppression.DO_NOT_DISTURB)
    assertEquals(PushDecision.ShowQuietCall(wakeUp), PushDecision.of(wakeUp))
  }

  @Test
  fun aWakeToSealShowsNothingAndAsksForTheWork() {
    assertEquals(PushDecision.SealScheduled, PushDecision.of(push(PushNotificationKind.SCHEDULED_SEAL)))
  }

  @Test
  fun aSyncWakeUpShowsNothingAndCatchesUp() {
    assertEquals(PushDecision.CatchUp, PushDecision.of(push(PushNotificationKind.SYNC)))
  }

  @Test
  fun aDismissWithACallStopsRinging() {
    val wakeUp = push(PushNotificationKind.DISMISS, maybeCallId = "77", maybeBadge = 3u)
    assertEquals(PushDecision.StopRinging("77", 3u), PushDecision.of(wakeUp))
  }

  @Test
  fun aDismissWithoutACallRemovesWhatWasRead() {
    val wakeUp = push(PushNotificationKind.DISMISS, maybeMessageId = "500", maybeThreadId = "400", maybeBadge = 0u)
    assertEquals(PushDecision.RemoveRead(CONVERSATION, "400", "500", 0u), PushDecision.of(wakeUp))
  }

  @Test
  fun aTestPushShowsTheConfirmation() {
    assertEquals(PushDecision.ShowTest(1_000L), PushDecision.of(push(PushNotificationKind.TEST, maybeSentAtMs = 1_000L)))
  }

  @Test
  fun anUnknownKindShowsNothing() {
    assertEquals(PushDecision.Ignore(IgnoreReason.UNKNOWN_KIND), PushDecision.of(push(PushNotificationKind.UNKNOWN)))
  }

  companion object {
    const val CONVERSATION = "100"

    fun push(
      kind: PushNotificationKind,
      maybeMessageId: String? = "200",
      maybeThreadId: String? = null,
      maybeCallId: String? = null,
      maybeBadge: UInt? = null,
      maybeSentAtMs: Long? = null,
      isSilent: Boolean = false,
      isUrgent: Boolean = false,
      isDuplicate: Boolean = false,
      maybeSuppression: NotificationSuppression? = null,
    ) = PushNotification(
      kind = kind,
      conversationId = CONVERSATION,
      maybeMessageId = maybeMessageId,
      maybeConversationName = "planning",
      maybeSenderId = "300",
      maybeSenderName = "Alice",
      maybeText = "hello",
      maybeFileName = null,
      maybeAttachmentKind = null,
      maybeGap = null,
      isRead = false,
      isDirect = false,
      isMention = false,
      isUrgent = isUrgent,
      isCall = kind == PushNotificationKind.CALL,
      isSilent = isSilent,
      isConversationVerified = true,
      maybeThreadId = maybeThreadId,
      maybeCallId = maybeCallId,
      maybeBadge = maybeBadge,
      maybeSentAtMs = maybeSentAtMs,
      isDuplicate = isDuplicate,
      maybeSuppression = maybeSuppression,
    )
  }
}
