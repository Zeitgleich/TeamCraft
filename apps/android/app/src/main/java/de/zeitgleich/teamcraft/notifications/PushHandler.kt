package de.zeitgleich.teamcraft.notifications

import android.content.Context
import de.zeitgleich.teamcraft.core.CatchUpWork
import de.zeitgleich.teamcraft.core.ScheduledSealWork
import android.os.SystemClock
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.update
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.calls.CallController
import de.zeitgleich.teamcraft.calls.IncomingRing
import de.zeitgleich.teamcraft.push.RetryTarget
import de.zeitgleich.teamcraft.push.WakeUpRetries
import uniffi.teamcraft_ffi.PushNotification

/** Carries out what a wake-up asks for ([PushDecision]): messages, rings, dismissals, tests. */
class PushHandler(
  private val context: Context,
  private val notifier: MessageNotifier,
  private val calls: CallController,
  private val stats: PushStats,
  private val elapsedNow: () -> Long = SystemClock::elapsedRealtime,
) : RetryTarget {
  private val handled = MutableStateFlow(0L)

  /** Increments with every wake-up handled; the diagnostics re-read on it. */
  val handledCount: StateFlow<Long> = handled

  /**
   * `payload`: the sealed wake-up, kept with a placeholder to read it later. `isVisible`: the push
   * service delivered it as one that must show something (FCM high priority). `isRingOver`: see
   * [PushDecision.of].
   */
  fun handle(notification: PushNotification, payload: ByteArray, isVisible: Boolean, isRingOver: Boolean = false) {
    when (val decision = PushDecision.of(notification, isRingOver)) {
      is PushDecision.Ignore -> stats.recordIgnored(decision.reason)
      is PushDecision.SealScheduled -> ScheduledSealWork.enqueue(context)
      is PushDecision.CatchUp -> CatchUpWork.enqueue(context)
      is PushDecision.ShowMessage -> {
        stats.recordPreview(decision.notification.maybeGap)
        stats.record(PushPath.of(decision.notification))
        val isUp = notifier.show(decision.notification, decision.isAlerting, payload)
        if (!isUp && isVisible) notifier.showQuietNote()
      }
      is PushDecision.HiddenChange -> {
        stats.record(PushPath.HIDDEN)
        // FCM lowers the priority of apps whose high-priority messages show nothing.
        if (!notifier.applyHiddenChange(decision.notification) && isVisible) notifier.showQuietNote()
      }
      is PushDecision.AlreadyRead -> {
        stats.record(PushPath.ALREADY_READ)
        if (!notifier.applyRead(decision.notification) && isVisible) notifier.showQuietNote()
      }
      is PushDecision.Ring -> calls.ring(ring(decision.conversationId, decision.maybeCallId, decision.notification))
      is PushDecision.RingAnswered -> {
        stats.record(PushPath.RING_ANSWERED)
        decision.maybeCallId?.let(calls::stopRinging)
        // FCM lowers the priority of apps whose high-priority messages show nothing.
        if (isVisible) notifier.showQuietNote()
      }
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
   * `path` says which one: fills in the message if it is still shown, silently if it was muted
   * meanwhile, or takes the line away if it was read or no message after all. Never posts anything
   * new. Alerts only if the first alert was held.
   */
  override fun handleRetry(notification: PushNotification, path: PushPath) {
    when (val decision = PushDecision.of(notification)) {
      is PushDecision.SealScheduled -> ScheduledSealWork.enqueue(context)
      is PushDecision.CatchUp -> CatchUpWork.enqueue(context)
      is PushDecision.ShowMessage -> if (notifier.fillIn(notification)) stats.record(path)
      is PushDecision.HiddenChange -> notifier.applyHiddenChange(notification)
      is PushDecision.AlreadyRead -> if (notifier.applyRead(notification)) stats.record(PushPath.ALREADY_READ)
      is PushDecision.Ignore -> {
        if (decision.reason == IgnoreReason.SUPPRESSED && notifier.fillIn(notification, isSuppressed = true)) stats.record(path)
      }
      else -> Unit
    }
  }

  /**
   * A wake-up that could not be opened at first ([MessageNotifier.showUnreadable]) opened now,
   * which is at least the first retry's delay later. A message shows without a second alert if a
   * generic one went out (`wasAnnounced`) and doesn't alert when it came too long ago; anything else
   * is treated as it arrives now, except that a ring is over.
   */
  override fun handleOpenedLate(notification: PushNotification, payload: ByteArray, wasAnnounced: Boolean, arrivedAtMs: Long) {
    when (val decision = PushDecision.of(notification, isRingOver = true)) {
      is PushDecision.ShowMessage -> {
        stats.record(PushPath.OPENED_LATE)
        val isAlerting = decision.isAlerting && !wasAnnounced && !PushRetry.isTooLateToAlert(arrivedAtMs, elapsedNow())
        notifier.showOpenedLate(decision.notification, isAlerting, payload)
      }
      else -> {
        notifier.cancelUnreadable()
        handle(notification, payload, isVisible = false, isRingOver = true)
      }
    }
  }

  override fun heldLines(): List<HeldLine> = notifier.heldLines()

  override fun endHold(payload: ByteArray, end: HoldEnd) {
    if (notifier.endHold(payload, end) && end == HoldEnd.ALERT) stats.record(PushPath.ALERT_RELEASED)
  }

  /** Whether this notification's message still shows as a placeholder (a retry is worth making). */
  override fun isPlaceholderShowing(notification: PushNotification): Boolean = notifier.isPlaceholderShowing(notification)

  override fun hasPlaceholderFor(payload: ByteArray): Boolean = notifier.hasPlaceholderFor(payload)

  /** A message the first try didn't read was read before anything was posted ([WakeUpRetries.settle]). */
  fun recordSettled() {
    stats.record(PushPath.FILLED_HOLD)
  }

  /** The last try for a placeholder failed; it stays until the sync brings the message. */
  override fun recordGaveUp() {
    stats.record(PushPath.GAVE_UP)
  }

  /** Too many chains of retries wait already: this one gets none. */
  override fun recordCapped() {
    stats.record(PushPath.RETRY_CAPPED)
  }

  /** A quiet wake-up that got no retry: the generic note goes out once, not nothing (issue #1256). */
  override fun announceUnopened() {
    stats.record(PushPath.UNREADABLE)
    notifier.showUnreadable()
    handled.update { it + 1 }
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
