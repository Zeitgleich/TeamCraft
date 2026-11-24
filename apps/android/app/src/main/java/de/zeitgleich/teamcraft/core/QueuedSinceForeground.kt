package de.zeitgleich.teamcraft.core

import java.util.concurrent.atomic.AtomicBoolean

/**
 * Whether the user queued something for the server (a message, a file, a reaction, any action)
 * since the app last left the foreground (#2358). Leaving the foreground then hands it to
 * [OutboxWork] at once, without asking the core what is pending first: that read can wait behind a
 * busy engine, and a swipe from the recent apps kills the process before it answers, so nothing
 * was scheduled and the message waited for the next start. A run that finds nothing to send ends
 * at once.
 */
class QueuedSinceForeground {
  private val isQueued = AtomicBoolean(false)

  /** Called before the action goes to the core, so a send that never returns still counts. */
  fun note() {
    isQueued.set(true)
  }

  /** Whether something was queued since the last call, which clears it. */
  fun take(): Boolean = isQueued.getAndSet(false)

  companion object {
    val shared = QueuedSinceForeground()
  }
}
