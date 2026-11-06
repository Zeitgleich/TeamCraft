package de.zeitgleich.teamcraft.push

import kotlin.coroutines.cancellation.CancellationException
import kotlinx.coroutines.delay
import kotlinx.coroutines.withTimeoutOrNull
import de.zeitgleich.teamcraft.core.NotSignedInException
import de.zeitgleich.teamcraft.notifications.HeldLine
import de.zeitgleich.teamcraft.notifications.HoldEnd
import de.zeitgleich.teamcraft.notifications.PushPath
import de.zeitgleich.teamcraft.notifications.PushRetry
import uniffi.teamcraft_ffi.PushNotification

/**
 * What happens after a wake-up could not be read at once: the tries before anything is posted
 * ([settle]), the later retries of a placeholder, and how a hold an earlier version left ends
 * (notifications.md, "Filling a placeholder in"). Apart from Android's classes, so failures,
 * timeouts and holds are unit-tested.
 *
 * A hold always ends: when the text comes, after any failed try, when the car goes, and at the next
 * process start. How it ends looks at the state then ([PushRetry.holdEnd]): a muted, read or old
 * message never alerts.
 */
class WakeUpRetries(
  private val opener: WakeUpOpener,
  private val target: RetryTarget,
  private val scheduler: RetryScheduler,
  private val isInCar: () -> Boolean,
  private val isAppVisible: () -> Boolean,
  private val isSigningOut: () -> Boolean,
  private val elapsedNow: () -> Long,
  /** Waits up to the given time for a usable network; whether there is one. */
  private val awaitNetwork: suspend (Long) -> Boolean,
  private val log: (String) -> Unit,
  private val pause: suspend (Long) -> Unit = { delay(it) },
) {
  /**
   * The first try could not read the message (the radio still waking up after Doze, a network
   * change, a busy server): tries again before anything is posted, each time once a network is
   * there, so the notification goes out once and with the text. A watch, a band and Android Auto
   * only ever take a message's first post; a "New message" filled in later never reaches them
   * (issue #1249). Bounded by [PushRetry.SETTLE_BOUND_MS], within the time a push service and the
   * expedited job give the app. Returns the newest result: still a placeholder when the bound passed.
   */
  suspend fun settle(payload: ByteArray, first: PushNotification): PushNotification {
    // Nothing shows yet: should the process die meanwhile, this later look shows the message. Its
    // attempt waits longer than the bound, so it never runs next to this wait.
    startChain(payload, RetryOf.UNOPENED_QUIET, SAFETY_ATTEMPT)
    val startedAtMs = elapsedNow()
    val deadlineMs = startedAtMs + PushRetry.SETTLE_BOUND_MS
    var latest = first
    var tries = 0
    while (PushRetry.isPlaceholder(latest) && !isSigningOut()) {
      val pauseMs = PushRetry.settleDelayMs(tries)
      if (elapsedNow() + pauseMs >= deadlineMs) break
      pause(pauseMs)
      if (!awaitNetwork(deadlineMs - elapsedNow())) break
      tries += 1
      val maybeAgain = withTimeoutOrNull((deadlineMs - elapsedNow()).coerceAtLeast(1)) {
        catching { opener.openSoon(payload) }.getOrNull()
      }
      maybeAgain?.let { latest = it }
    }
    // The gap is logged, never the content (notifications.md, "Mobile push delivery").
    log("message not read at once (${first.maybeGap}): $tries more tries in ${elapsedNow() - startedAtMs} ms, now ${latest.maybeGap}")
    scheduler.cancelRetry(payload, SAFETY_ATTEMPT)
    return latest
  }

  /** A placeholder went out after [settle]: later retries fill it in, within the bound on chains. */
  fun afterPlaceholder(payload: ByteArray) {
    startChain(payload, RetryOf.PLACEHOLDER)
  }

  /** A wake-up that did not open: later tries, within the device's bound on chains. */
  fun afterNotOpened(payload: ByteArray, wasAnnounced: Boolean) {
    startChain(payload, RetryOf.of(wasAnnounced))
  }

  /**
   * One retry. Schedules the next while it still can't be read, also after a failure: a store that
   * was busy or a core that was closed is no reason to stop. Only being signed out ends the chain.
   * A placeholder that went meanwhile (filled in, read, swiped away) costs nothing more.
   */
  suspend fun retry(payload: ByteArray, attempt: Int, retryOf: RetryOf, arrivedAtMs: Long) {
    if (isSigningOut()) return
    if (retryOf == RetryOf.PLACEHOLDER && !target.hasPlaceholderFor(payload)) return
    catching { opener.openLate(payload) }
      .onSuccess { opened ->
        if (isSigningOut()) return
        when (retryOf) {
          RetryOf.PLACEHOLDER -> target.handleRetry(opened, PushPath.FILLED_RETRY)
          RetryOf.UNOPENED_ANNOUNCED -> target.handleOpenedLate(opened, payload, wasAnnounced = true, arrivedAtMs)
          RetryOf.UNOPENED_QUIET -> target.handleOpenedLate(opened, payload, wasAnnounced = false, arrivedAtMs)
        }
        afterRetry(payload, opened, attempt, arrivedAtMs)
      }
      .onFailure { failure ->
        log("wake-up retry $attempt failed: ${failure.javaClass.simpleName}")
        if (failure !is NotSignedInException) onFailed(payload, attempt, retryOf, arrivedAtMs)
      }
  }

  /** An attempt didn't finish in its time: the next one takes over. */
  fun onTimedOut(payload: ByteArray, attempt: Int, retryOf: RetryOf, arrivedAtMs: Long) {
    if (!isSigningOut()) onFailed(payload, attempt, retryOf, arrivedAtMs)
  }

  /**
   * Ends holds past their bound, looking first whether the sync brought the text. `isForced`: all
   * of them (the car went away). Returns whether any is left, so the job looks again.
   */
  suspend fun endHolds(isForced: Boolean = false): Boolean {
    target.heldLines()
      .filter { line -> isForced || isDue(line) }
      .forEach { line ->
        val maybeOpened = catching { opener.fromStore(line.payload) }.getOrNull()
        maybeOpened?.let { target.handleRetry(it, PushPath.FILLED_STORE) }
        endHold(line.payload, maybeOpened)
      }
    return target.heldLines().isNotEmpty()
  }

  private fun afterRetry(payload: ByteArray, opened: PushNotification, attempt: Int, arrivedAtMs: Long) {
    if (!target.isPlaceholderShowing(opened)) return
    if (!isInCar()) endHold(payload, opened)
    if (PushRetry.isWanted(opened, attempt + 1)) {
      // The gap is logged, never the content (notifications.md, "Mobile push delivery").
      log("message not read in time (${opened.maybeGap}); retry ${attempt + 1} scheduled")
      scheduler.enqueueRetry(payload, attempt + 1, RetryOf.PLACEHOLDER, arrivedAtMs)
    } else {
      endHold(payload, opened)
      target.recordGaveUp()
    }
  }

  private fun onFailed(payload: ByteArray, attempt: Int, retryOf: RetryOf, arrivedAtMs: Long) {
    endHold(payload, maybeOpened = null)
    if (attempt < PushRetry.MAX_ATTEMPTS) {
      scheduler.enqueueRetry(payload, attempt + 1, retryOf, arrivedAtMs)
    } else {
      target.recordGaveUp()
    }
  }

  private fun endHold(payload: ByteArray, maybeOpened: PushNotification?) {
    val maybeLine = target.heldLines().firstOrNull { it.payload.contentEquals(payload) } ?: return
    target.endHold(payload, PushRetry.holdEnd(maybeOpened, maybeLine.maybeArrivedAtMs, elapsedNow(), isAppVisible()))
  }

  private fun isDue(line: HeldLine): Boolean {
    val arrivedAtMs = line.maybeArrivedAtMs ?: return true
    val heldMs = elapsedNow() - arrivedAtMs
    return heldMs < 0 || heldMs >= PushRetry.holdBoundMs(isInCar())
  }

  private fun startChain(payload: ByteArray, retryOf: RetryOf, attempt: Int = 1) {
    if (scheduler.pendingChains() >= scheduler.maxChains()) {
      // A broken store must not pile up jobs; the sync still fills in what it brings.
      target.recordCapped()
    } else {
      scheduler.enqueueRetry(payload, attempt, retryOf, elapsedNow())
    }
  }

  private companion object {
    /** The attempt of the look kept during [settle]: its delay (10 min) is past [PushRetry.SETTLE_BOUND_MS]. */
    const val SAFETY_ATTEMPT = 3
  }
}

/** Opens a wake-up that was opened before: not counted as received. */
interface WakeUpOpener {
  /** May fetch and catch up; for a try with a tight bound, so it doesn't ask for the read position. */
  suspend fun openSoon(payload: ByteArray): PushNotification

  /** May fetch and catch up, and asks the server how far the user has read: can take seconds more. */
  suspend fun openLate(payload: ByteArray): PushNotification

  /** Only what the store holds; no network. */
  suspend fun fromStore(payload: ByteArray): PushNotification
}

/** What the retries do to what is shown. */
interface RetryTarget {
  fun handleRetry(notification: PushNotification, path: PushPath)
  fun handleOpenedLate(notification: PushNotification, payload: ByteArray, wasAnnounced: Boolean, arrivedAtMs: Long)
  fun isPlaceholderShowing(notification: PushNotification): Boolean
  fun hasPlaceholderFor(payload: ByteArray): Boolean
  fun heldLines(): List<HeldLine>
  fun endHold(payload: ByteArray, end: HoldEnd)
  fun recordGaveUp()
  fun recordCapped()
}

/** The persisted jobs behind the retries. `arrivedAtMs` is `elapsedRealtime`. */
interface RetryScheduler {
  fun enqueueRetry(payload: ByteArray, attempt: Int, retryOf: RetryOf, arrivedAtMs: Long)

  /** Takes back `attempt` of this wake-up's retries if it hasn't run yet. */
  fun cancelRetry(payload: ByteArray, attempt: Int)

  /** Chains of retries waiting now. */
  fun pendingChains(): Int

  /** The most chains waiting at a time (managed configuration, else [PushRetry.DEFAULT_MAX_CHAINS]). */
  fun maxChains(): Int
}

/** What a retry of a wake-up is for, and whether something was said for it yet. */
enum class RetryOf {
  /** A message shown as a placeholder. */
  PLACEHOLDER,

  /** A wake-up that did not open; a generic "New message" went out for it. */
  UNOPENED_ANNOUNCED,

  /** A wake-up that did not open, and nothing showed (it may have been a withdrawal). */
  UNOPENED_QUIET,
  ;

  companion object {
    fun of(wasAnnounced: Boolean): RetryOf = if (wasAnnounced) UNOPENED_ANNOUNCED else UNOPENED_QUIET
  }
}

/** [runCatching] that lets a cancellation through: a cancelled job must not count as a failed try. */
internal inline fun <T> catching(block: () -> T): Result<T> =
  try {
    Result.success(block())
  } catch (cancelled: CancellationException) {
    throw cancelled
  } catch (failure: Throwable) {
    Result.failure(failure)
  }
