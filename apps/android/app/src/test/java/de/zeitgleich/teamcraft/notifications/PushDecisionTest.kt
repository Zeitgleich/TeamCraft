package de.zeitgleich.teamcraft.notifications

import org.junit.Assert.assertEquals
import org.junit.Test
import uniffi.teamcraft_ffi.NotificationSuppression
import uniffi.teamcraft_ffi.PushNotification
import uniffi.teamcraft_ffi.PushNotificationKind

class PushDecisionTest {
  @Test
  fun aNewMessageAlerts() {
    val wakeUp = push(PushNotificationKind.MESSAGE)
    assertEquals(PushDecision.ShowMessage(wakeUp, isAlerting = true), PushDecision.of(wakeUp))
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
