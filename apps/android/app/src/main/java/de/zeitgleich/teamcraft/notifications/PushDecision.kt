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
  /** A scheduled message's group changed: seal it again (docs/scheduled-messages.md). Shows nothing. */
  data object SealScheduled : PushDecision

  /** Post nothing: muted or DND (the device's own check), or an unknown kind. */
  data class Ignore(val reason: IgnoreReason) : PushDecision

  /** Add the message to its conversation's notification; `isAlerting`: sound and heads-up. */
  data class ShowMessage(val notification: PushNotification, val isAlerting: Boolean) : PushDecision

  /**
   * A reaction, an edit or another change to the conversation: the server can't tell it from a
   * message and woke the device for it, but there is nothing to announce (issue #1249).
   */
  data class HiddenChange(val notification: PushNotification) : PushDecision

  /** Read before this wake-up was opened: takes away what shows of it, announces nothing. */
  data class AlreadyRead(val notification: PushNotification) : PushDecision

  /** Ring: incoming-call notification with full-screen intent. */
  data class Ring(val conversationId: String, val maybeCallId: String?, val notification: PushNotification) : PushDecision

  /**
   * A ring looked at late that the server's call history says this user answered on another device
   * (issue #1257): no missed call, and a ring of it still going here stops.
   */
  data class RingAnswered(val maybeCallId: String?) : PushDecision

  /**
   * A ring the device's own mute/DND check keeps quiet, or one opened after it ended without the
   * server saying it was answered: a silent missed-call notification.
   */
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
    /**
     * `isRingOver`: this wake-up was opened so late that a ring in it has ended by now; it is told
     * as a missed call, unless the server says it was answered elsewhere ([RingAnswered]), and never
     * rings or reaches Telecom.
     */
    fun of(notification: PushNotification, isRingOver: Boolean = false): PushDecision = when (notification.kind) {
      PushNotificationKind.MESSAGE -> message(notification)
      PushNotificationKind.CALL -> call(notification, isRingOver)
      PushNotificationKind.DISMISS -> dismiss(notification)
      PushNotificationKind.SCHEDULED_SEAL -> SealScheduled
      PushNotificationKind.TEST -> ShowTest(notification.maybeSentAtMs)
      PushNotificationKind.UNKNOWN -> Ignore(IgnoreReason.UNKNOWN_KIND)
    }

    private fun message(notification: PushNotification): PushDecision = when {
      notification.maybeSuppression != null -> Ignore(IgnoreReason.SUPPRESSED)
      notification.maybeGap == NotificationGap.HIDDEN -> HiddenChange(notification)
      // An urgent repeat is meant to alert until it is acknowledged.
      notification.isRead && !notification.isUrgent -> AlreadyRead(notification)
      // Shown before: update the notification without alerting (urgent repeats are never
      // duplicates; they replace the line and alert again).
      notification.isDuplicate -> ShowMessage(notification, isAlerting = false)
      else -> ShowMessage(notification, isAlerting = !notification.isSilent)
    }

    private fun call(notification: PushNotification, isRingOver: Boolean): PushDecision = when {
      // Only a late look asks the server, so a ring that arrives in time always rings.
      notification.isRingAnswered -> RingAnswered(notification.maybeCallId)
      notification.maybeSuppression != null || isRingOver -> ShowQuietCall(notification)
      else -> Ring(notification.conversationId, notification.maybeCallId, notification)
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

  /**
   * The longest the try right away may take: with the first try (at most 7 s, the core's budget),
   * how long an alert is held off the road. Short, because a wake-up past the receiver's 8 s costs
   * expedited quota; a try that takes longer isn't likely to read it anyway.
   */
  const val HOLD_TRY_MS = 5_000L

  /** Chains of retries that may wait at a time, unless the device's management says otherwise. */
  const val DEFAULT_MAX_CHAINS = 32

  /** The longest the first try and the try right away may hold an alert, with room to spare. */
  private const val HOLD_BOUND_MS = 20_000L

  /** The longest an alert waits while the phone projects to a car (no network needed to end it). */
  private const val HOLD_BOUND_IN_CAR_MS = 2 * 60_000L

  /** A burst of wake-ups with nothing to show puts up one quiet note; a minute is one burst. */
  private const val QUIET_NOTE_GAP_MS = 60_000L

  /** After this, a message doesn't alert any more when it gets its text or its hold ends. */
  private const val MAX_LATE_ALERT_MS = 5 * 60_000L

  /** The server's ring lasts 30 s by default, this device's 45 s: past this, the ring is over. */
  private const val RING_OVER_AFTER_MS = 45_000L

  /** Before these attempts, by attempt: the first ones for a slow moment, the last for a long one. */
  private val DELAYS_MS = longArrayOf(30_000L, 2 * 60_000L, 10 * 60_000L, 60 * 60_000L)
  private val READABLE_LATER = setOf(NotificationGap.TIMED_OUT, NotificationGap.NOT_DECRYPTED, NotificationGap.NO_SESSION)

  /** The notification shows a placeholder for a message that may be readable later. */
  fun isPlaceholder(notification: PushNotification): Boolean =
    notification.kind == PushNotificationKind.MESSAGE &&
      notification.maybeMessageId != null &&
      notification.maybeGap in READABLE_LATER

  /** Whether `attempt` (1 = the first retry) should be made for what this try showed. */
  fun isWanted(notification: PushNotification, attempt: Int): Boolean =
    attempt <= MAX_ATTEMPTS && isPlaceholder(notification)

  /** How long `attempt` waits (it may be longer: WorkManager's jobs wait for Doze windows). */
  fun delayMs(attempt: Int): Long = DELAYS_MS[(attempt - 1).coerceIn(0, DELAYS_MS.lastIndex)]

  /** Whether a quiet note may go up now, after the last one at `maybeLastAtMs`. */
  fun isQuietNoteDue(maybeLastAtMs: Long?, nowMs: Long): Boolean =
    maybeLastAtMs == null || nowMs - maybeLastAtMs >= QUIET_NOTE_GAP_MS

  /** How long an alert may be held. */
  fun holdBoundMs(isInCar: Boolean): Long = if (isInCar) HOLD_BOUND_IN_CAR_MS else HOLD_BOUND_MS

  /**
   * Whether it is too late to alert for a wake-up that came at `maybeArrivedAtMs`. Both on
   * `elapsedRealtime`, so a wrong clock doesn't matter; earlier than that means the phone restarted
   * since, and an unknown time counts as late.
   */
  fun isTooLateToAlert(maybeArrivedAtMs: Long?, nowMs: Long): Boolean {
    val arrivedAtMs = maybeArrivedAtMs ?: return true
    return nowMs < arrivedAtMs || nowMs - arrivedAtMs > MAX_LATE_ALERT_MS
  }

  /** How a hold ends, from what is known now; `maybeOpened` is `null` when the wake-up didn't open. */
  fun holdEnd(maybeOpened: PushNotification?, maybeArrivedAtMs: Long?, nowMs: Long, isAppVisible: Boolean): HoldEnd =
    when {
      maybeOpened?.isRead == true -> HoldEnd.DROP
      maybeOpened?.maybeSuppression != null -> HoldEnd.SILENT
      isAppVisible || isTooLateToAlert(maybeArrivedAtMs, nowMs) -> HoldEnd.SILENT
      else -> HoldEnd.ALERT
    }

  /**
   * Whether a ring in a wake-up sent at `maybeSentAtMs` (the server's time) is over at `nowMs` (this
   * device's). Only asked of a wake-up reopened after its first run; a clock far ahead costs a ring
   * there, told as a missed call. A first delivery always rings: the push service's time to live is
   * the ring's own, so a wrong clock can't silence a real call.
   */
  fun isRingOver(maybeSentAtMs: Long?, nowMs: Long): Boolean =
    maybeSentAtMs != null && nowMs - maybeSentAtMs > RING_OVER_AFTER_MS
}

/** How a held alert ends. */
enum class HoldEnd {
  /** The phone alerts now, without the text. */
  ALERT,

  /** Muted meanwhile, too old, or the user is in the app: it stays, without a sound. */
  SILENT,

  /** Read meanwhile: the line goes. */
  DROP,
}

enum class IgnoreReason {
  SUPPRESSED,
  UNKNOWN_KIND,
}
