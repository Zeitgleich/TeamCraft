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

  fun handle(notification: PushNotification) {
    when (val decision = PushDecision.of(notification)) {
      is PushDecision.Ignore -> stats.recordIgnored(decision.reason)
      is PushDecision.ShowMessage -> {
        stats.recordPreview(decision.notification.maybeGap)
        notifier.show(decision.notification, decision.isAlerting)
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
   * A retry of a wake-up shown as a placeholder ([PushRetry]): fills in the message if it is still
   * shown; never alerts, never posts anything new.
   */
  fun handleRetry(notification: PushNotification) {
    if (PushDecision.of(notification) is PushDecision.ShowMessage) notifier.fillIn(notification)
  }

  /** Whether a notification of this conversation shows now (a retry for it is worth making). */
  fun isShowing(conversationId: String): Boolean = conversationId in notifier.shownConversationIds()

  /** A visible wake-up that could not be opened: never nothing (see [MessageNotifier.showUnreadable]). */
  fun handleUnreadable() {
    stats.recordUnreadable()
    notifier.showUnreadable()
    handled.update { it + 1 }
  }

  /** A wake-up that could not be opened and must not show anything (it may have been a withdrawal). */
  fun recordUnreadable() {
    stats.recordUnreadable()
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
