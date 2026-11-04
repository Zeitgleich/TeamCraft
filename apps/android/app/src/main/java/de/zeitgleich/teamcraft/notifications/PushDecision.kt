package de.zeitgleich.teamcraft.notifications

import uniffi.teamcraft_ffi.NotificationGap
import uniffi.teamcraft_ffi.PushNotification
import uniffi.teamcraft_ffi.PushNotificationKind

/**
 * What the app does with an opened wake-up (notifications.md, "Mobile push delivery"). Pure, so
 * every kind and flag is unit-tested; [PushHandler] carries it out.
 *
 * FCM lowers the priority of apps whose high-priority messages show nothing, so every wake-up the
 * server sends with high priority (message, call, test) shows or updates a notification, except
 * when the device's own mute/DND check says no (rare: the server checked first; counted for the
 * diagnostics). `dismiss` (normal priority) shows nothing; [HiddenChange] updates what is shown, or
 * shows a short quiet note.
 */
sealed interface PushDecision {
  /** Post nothing: muted or DND (the device's own check), or an unknown kind. */
  data class Ignore(val reason: IgnoreReason) : PushDecision

  /** Add the message to its conversation's notification; `isAlerting`: sound and heads-up. */
  data class ShowMessage(val notification: PushNotification, val isAlerting: Boolean) : PushDecision

  /**
   * A reaction, an edit or another change to the conversation: the server can't tell it from a
   * message and woke the device for it, but there is nothing to announce (issue #1249).
   */
  data class HiddenChange(val notification: PushNotification) : PushDecision

  /** Ring: incoming-call notification with full-screen intent. */
  data class Ring(val conversationId: String, val maybeCallId: String?, val notification: PushNotification) : PushDecision

  /** A ring the device's own mute/DND check keeps quiet: a silent missed-call notification. */
  data class ShowQuietCall(val notification: PushNotification) : PushDecision

  /** The ring ended elsewhere (answered, declined, timed out): stop ringing. */
  data class StopRinging(val callId: String, val maybeBadge: UInt?) : PushDecision

  /** Read on another device: remove those lines (the thread's, or the conversation's). */
  data class RemoveRead(
    val conversationId: String,
    val maybeThreadId: String?,
    val maybeUpToMessageId: String?,
    val maybeBadge: UInt?,
  ) : PushDecision

  /** The diagnostics' test push arrived: a short confirmation. */
  data class ShowTest(val maybeSentAtMs: Long?) : PushDecision

  companion object {
    fun of(notification: PushNotification): PushDecision = when (notification.kind) {
      PushNotificationKind.MESSAGE -> message(notification)
      PushNotificationKind.CALL -> call(notification)
      PushNotificationKind.DISMISS -> dismiss(notification)
      PushNotificationKind.TEST -> ShowTest(notification.maybeSentAtMs)
      PushNotificationKind.UNKNOWN -> Ignore(IgnoreReason.UNKNOWN_KIND)
    }

    private fun message(notification: PushNotification): PushDecision = when {
      notification.maybeSuppression != null -> Ignore(IgnoreReason.SUPPRESSED)
      notification.maybeGap == NotificationGap.HIDDEN -> HiddenChange(notification)
      // Shown before: update the notification without alerting (urgent repeats are never
      // duplicates; they replace the line and alert again).
      notification.isDuplicate -> ShowMessage(notification, isAlerting = false)
      else -> ShowMessage(notification, isAlerting = !notification.isSilent)
    }

    private fun call(notification: PushNotification): PushDecision =
      if (notification.maybeSuppression != null) {
        ShowQuietCall(notification)
      } else {
        Ring(notification.conversationId, notification.maybeCallId, notification)
      }

    private fun dismiss(notification: PushNotification): PushDecision {
      notification.maybeCallId?.let { return StopRinging(it, notification.maybeBadge) }
      return RemoveRead(
        conversationId = notification.conversationId,
        maybeThreadId = notification.maybeThreadId,
        maybeUpToMessageId = notification.maybeMessageId,
        maybeBadge = notification.maybeBadge,
      )
    }
  }
}

/**
 * A message notification shown before its text could be read (the fetch timed out, decryption
 * failed or no session): the app tries again right away while the alert is held back, then opens
 * the same wake-up again later and updates the notification under the same ID (issues #739,
 * #1249). Each wait is for a network too. Policy or setting cuts are not retried. Bounded: only
 * while the placeholder shows, and with the last try the placeholder stays until it is read, the
 * sync brings the message ([PlaceholderWatcher]) or the notification goes.
 */
object PushRetry {
  /** Tries after the one right away; each waits for a network. */
  const val MAX_ATTEMPTS = 4

  /** The longest the try right away may take: with the first try, how long an alert is held. */
  const val HOLD_TRY_MS = 8_000L

  /** Before these attempts, by attempt: the first ones for a slow moment, the last for a long one. */
  private val DELAYS_MS = longArrayOf(30_000L, 2 * 60_000L, 10 * 60_000L, 60 * 60_000L)

  /** While the phone projects to a car, the alert stays held until this attempt failed. */
  private const val LAST_HELD_ATTEMPT_IN_CAR = 2
  private val READABLE_LATER = setOf(NotificationGap.TIMED_OUT, NotificationGap.NOT_DECRYPTED, NotificationGap.NO_SESSION)

  /** The notification shows a placeholder for a message that may be readable later. */
  fun isPlaceholder(notification: PushNotification): Boolean =
    notification.kind == PushNotificationKind.MESSAGE &&
      notification.maybeMessageId != null &&
      notification.maybeGap in READABLE_LATER

  /** Whether `attempt` (1 = the first retry) should be made for what this try showed. */
  fun isWanted(notification: PushNotification, attempt: Int): Boolean =
    attempt <= MAX_ATTEMPTS && notification.maybeSuppression == null && isPlaceholder(notification)

  /** How long `attempt` waits (it may be longer: WorkManager's jobs wait for Doze windows). */
  fun delayMs(attempt: Int): Long = DELAYS_MS[(attempt - 1).coerceIn(0, DELAYS_MS.lastIndex)]

  /**
   * Whether a held alert is given up after `attempt` failed; 0 is the try right away. In a car the
   * alert waits longer: a phone sound doesn't reach the driver the way an announcement does.
   */
  fun releasesHeldAlert(attempt: Int, isInCar: Boolean): Boolean =
    !isInCar || attempt >= LAST_HELD_ATTEMPT_IN_CAR
}

enum class IgnoreReason {
  SUPPRESSED,
  UNKNOWN_KIND,
}
