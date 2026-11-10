package de.zeitgleich.teamcraft.ui

import java.io.IOException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch
import uniffi.teamcraft_ffi.PictureRetryRounds
import uniffi.teamcraft_ffi.TeamCraftException

/** The core's retry arithmetic (`PictureRetryRounds`), as [PictureRetries] uses it; a fake in tests. */
interface RetryRounds {
  /** `null`: part of the planned round; else the wait in ms before the round runs. */
  fun failed(maybeServerWaitMs: Long?): Long?

  fun roundStarted()

  fun succeeded()

  fun restart()
}

/** The rounds of the core's policy: 5 seconds doubling up to 5 minutes, with jitter (issue #743). */
class CoreRetryRounds(private val rounds: PictureRetryRounds = PictureRetryRounds()) : RetryRounds {
  override fun failed(maybeServerWaitMs: Long?): Long? = rounds.failed(maybeServerWaitMs?.toULong())?.toLong()

  override fun roundStarted() = rounds.roundStarted()

  override fun succeeded() = rounds.succeeded()

  override fun restart() = rounds.restart()
}

/**
 * When pictures that failed during an outage are asked for again (issue #1616), as on the web and
 * the desktop: one pause for all of them in place of a timer per picture, with the core's capped
 * exponential backoff. A picture that failed for a reason that passes calls [failed]; the screens
 * read [round] and ask again when it moves. The end of the device's outage ([isOnline] going from
 * false to true) runs the round at once and starts the backoff over. Nothing runs while no picture
 * waits: no polling. What is on screen meanwhile (the cached picture, the initials) stays.
 */
class PictureRetries(
  private val scope: CoroutineScope,
  isOnline: StateFlow<Boolean>,
  private val rounds: RetryRounds,
) {
  private val lock = Any()
  private val roundCount = MutableStateFlow(0)
  private var maybeTimer: Job? = null
  private var isWaiting = false

  /** Counts the rounds that ran: a picture that failed is asked for again when this moves. */
  val round: StateFlow<Int> = roundCount

  init {
    scope.launch {
      var wasOnline = isOnline.value
      isOnline.collect { now ->
        if (now && !wasOnline) networkRestored()
        wasOnline = now
      }
    }
  }

  /**
   * A picture did not load for a reason that passes ([isTransient]); `maybeServerWaitMs` is what the
   * server asked to wait, if it said.
   */
  fun failed(maybeServerWaitMs: Long? = null) {
    synchronized(lock) {
      isWaiting = true
      val wait = rounds.failed(maybeServerWaitMs) ?: return
      maybeTimer = scope.launch {
        delay(wait)
        startRound()
      }
    }
  }

  /** A picture came through: the next failure starts from the shortest wait again. */
  fun succeeded() {
    synchronized(lock) { rounds.succeeded() }
  }

  private fun startRound() {
    synchronized(lock) {
      maybeTimer = null
      isWaiting = false
      rounds.roundStarted()
      roundCount.value++
    }
  }

  private fun networkRestored() {
    synchronized(lock) {
      if (!isWaiting) return
      maybeTimer?.cancel()
      maybeTimer = null
      isWaiting = false
      rounds.restart()
      roundCount.value++
    }
  }
}

/** Trouble that passes (no connection, a storage outage, 5xx, 429); anything else is final for now. */
fun isTransient(failure: Exception): Boolean = failure is TeamCraftException.Network || failure is IOException
