package de.zeitgleich.teamcraft.ui

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch

/**
 * Runs a read for "something changed" signals so a burst of them costs at most two reads: the one
 * running, and one more if a signal came in meanwhile (issue #1900). A signal while a read runs
 * only marks it dirty; it never queues another read behind the first. The run is not tied to the
 * caller, so a screen that recomposes does not cancel it and leave its job queued in the core.
 *
 * Main-thread confined: call [request] from the main thread, and run on a main-confined [scope].
 */
class ConflatedRefresh(private val scope: CoroutineScope, private val read: suspend () -> Unit) {
  private var isRunning = false
  private var isDirty = false

  /** Asks for a read; starts one now, or marks the running one to read again once it is done. */
  fun request() {
    if (isRunning) {
      isDirty = true
      return
    }
    isRunning = true
    scope.launch {
      try {
        do {
          isDirty = false
          read()
        } while (isDirty)
      } finally {
        isRunning = false
      }
    }
  }
}
