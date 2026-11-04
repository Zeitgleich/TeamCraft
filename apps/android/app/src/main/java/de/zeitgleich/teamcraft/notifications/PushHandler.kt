package de.zeitgleich.teamcraft.notifications

import android.content.Context
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.update
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.calls.CallController
import de.zeitgleich.teamcraft.calls.IncomingRing
import uniffi.teamcraft_ffi.PushNotification

/** Carries out what a wake-up asks for ([PushDecision]): messages, rings, dismissals, tests. */
class PushHandler(
  private val context: Context,
  private val notifier: MessageNotifier,
  private val calls: CallController,
  private val stats: PushStats,
) {
  private val handled = MutableStateFlow(0L)

  /** Increments with every wake-up handled; the diagnostics re-read on it. */
  val handledCount: StateFlow<Long> = handled

  /**
   * `payload`: the sealed wake-up, kept with a placeholder to read it later. `isVisible`: the push
   * service delivered it as one that must show something (FCM high priority).
   */
  fun handle(notification: PushNotification, payload: ByteArray, isVisible: Boolean) {
    when (val decision = PushDecision.of(notification)) {
      is PushDecision.Ignore -> stats.recordIgnored(decision.reason)
      is PushDecision.ShowMessage -> {
        stats.recordPreview(decision.notification.maybeGap)
        stats.record(PushPath.of(decision.notification))
        notifier.show(decision.notification, decision.isAlerting, payload)
      }
      is PushDecision.HiddenChange -> {
        stats.record(PushPath.HIDDEN)
        // FCM lowers the priority of apps whose high-priority messages show nothing.
        if (!notifier.applyHiddenChange(decision.notification) && isVisible) notifier.showQuietNote()
      }
      is PushDecision.Ring -> calls.ring(ring(decision.conversationId, decision.maybeCallId, decision.notification))
      is PushDecision.ShowQuietCall -> {
        stats.recordIgnored(IgnoreReason.SUPPRESSED)
        notifier.showQuietCall(ring(decision.notification.conversationId, null, decision.notification).displayName)
      }
      is PushDecision.StopRinging -> calls.stopRinging(decision.callId)
      is PushDecision.RemoveRead -> notifier.removeRead(
        conversationId = decision.conversationId,
        maybeThreadId = decision.maybeThreadId,
        maybeUpToMessageId = decision.maybeUpToMessageId,
        maybeBadge = decision.maybeBadge,
      )
      is PushDecision.ShowTest -> notifier.showTest(decision.maybeSentAtMs)
    }
    handled.update { it + 1 }
  }

  /**
   * A later look at a wake-up shown as a placeholder ([PushRetry], [PlaceholderWatcher]), where
   * `path` says which one: fills in the message if it is still shown, or takes the line away if it
   * was no message after all. Never posts anything new. Alerts only if the first alert was held.
   */
  fun handleRetry(notification: PushNotification, path: PushPath) {
    when (PushDecision.of(notification)) {
      is PushDecision.ShowMessage -> if (notifier.fillIn(notification)) stats.record(path)
      is PushDecision.HiddenChange -> notifier.applyHiddenChange(notification)
      else -> Unit
    }
  }

  /**
   * A wake-up that could not be opened at first ([MessageNotifier.showUnreadable]) opened now: it
   * shows like a new message, without a second alert if a generic one went out (`wasAnnounced`).
   */
  fun handleOpenedLate(notification: PushNotification, payload: ByteArray, wasAnnounced: Boolean) {
    when (val decision = PushDecision.of(notification)) {
      is PushDecision.ShowMessage -> {
        stats.record(PushPath.OPENED_LATE)
        notifier.showOpenedLate(decision.notification, decision.isAlerting && !wasAnnounced, payload)
      }
      // A withdrawal or a change that came late: nothing is lost by treating it as it arrives now.
      else -> handle(notification, payload, isVisible = false)
    }
  }

  /** Gives up holding this message's alert: it alerts on the phone, without the text. */
  fun releaseHeldAlert(notification: PushNotification) {
    if (notifier.releaseHeldAlert(notification)) stats.record(PushPath.ALERT_RELEASED)
  }

  /** Whether this notification's message still shows as a placeholder (a retry is worth making). */
  fun isPlaceholderShowing(notification: PushNotification): Boolean = notifier.isPlaceholderShowing(notification)

  /** The last try for a placeholder failed; it stays until the sync brings the message. */
  fun recordGaveUp() {
    stats.record(PushPath.GAVE_UP)
  }

  /** A visible wake-up that could not be opened: never nothing (see [MessageNotifier.showUnreadable]). */
  fun handleUnreadable() {
    stats.recordUnreadable()
    stats.record(PushPath.UNREADABLE)
    notifier.showUnreadable()
    handled.update { it + 1 }
  }

  /** A wake-up that could not be opened and must not show anything (it may have been a withdrawal). */
  fun recordUnreadable() {
    stats.recordUnreadable()
    stats.record(PushPath.UNREADABLE_QUIET)
  }

  private fun ring(conversationId: String, maybeCallId: String?, notification: PushNotification): IncomingRing {
    val displayName = notification.maybeConversationName?.takeIf { it.isNotBlank() }
      ?: notification.maybeSenderName?.takeIf { it.isNotBlank() }
      ?: context.getString(R.string.app_name)
    return IncomingRing(
      conversationId = conversationId,
      maybeCallId = maybeCallId,
      displayName = displayName,
      maybeCallerName = notification.maybeSenderName?.takeIf { it.isNotBlank() },
      isVerified = notification.isConversationVerified,
    )
  }
}
