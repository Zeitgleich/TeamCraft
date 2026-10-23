package de.zeitgleich.teamcraft.notifications

import uniffi.teamcraft_ffi.PushNotification
import uniffi.teamcraft_ffi.PushNotificationKind

/**
 * What the app does with an opened wake-up (notifications.md, "Mobile push delivery"). Pure, so
 * every kind and flag is unit-tested; [PushHandler] carries it out.
 *
 * FCM lowers the priority of apps whose high-priority messages show nothing, so every wake-up the
 * server sends with high priority (message, call, test) shows or updates a notification, except
 * when the device's own mute/DND check says no (rare: the server checked first; counted for the
 * diagnostics). Only `dismiss` (normal priority) shows nothing.
 */
sealed interface PushDecision {
  /** Post nothing: muted or DND (the device's own check), or an unknown kind. */
  data class Ignore(val reason: IgnoreReason) : PushDecision

  /** Add the message to its conversation's notification; `isAlerting`: sound and heads-up. */
  data class ShowMessage(val notification: PushNotification, val isAlerting: Boolean) : PushDecision

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

enum class IgnoreReason {
  SUPPRESSED,
  UNKNOWN_KIND,
}
