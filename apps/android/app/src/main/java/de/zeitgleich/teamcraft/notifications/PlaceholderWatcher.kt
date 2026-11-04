package de.zeitgleich.teamcraft.notifications

import java.util.TimeZone
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.FlowPreview
import kotlinx.coroutines.flow.debounce
import kotlinx.coroutines.flow.drop
import kotlinx.coroutines.launch
import de.zeitgleich.teamcraft.core.CoreRepository

/**
 * Fills in placeholders from what the sync stored (issue #1249), however long after the retries
 * ([PushRetry]) ended: while the process lives, on every change of the store, and when it starts,
 * also after a restart. Reads only the store and only while a placeholder shows: no network, no
 * timer, and no work at all in the usual case without a placeholder.
 */
class PlaceholderWatcher(
  private val core: CoreRepository,
  private val notifier: MessageNotifier,
  private val handler: PushHandler,
  private val scope: CoroutineScope,
) {
  @OptIn(FlowPreview::class)
  fun start() {
    scope.launch {
      notifier.releaseEarlierHolds()
      fillIn()
      core.changeCount.drop(1).debounce(DEBOUNCE_MS).collect { fillIn() }
    }
  }

  private suspend fun fillIn() {
    val payloads = notifier.placeholderPayloads()
    if (payloads.isEmpty()) return
    val opened = runCatching { core.core() }.getOrNull() ?: return
    val offsetMinutes = (TimeZone.getDefault().getOffset(System.currentTimeMillis()) / MS_PER_MINUTE).toShort()
    payloads.forEach { payload ->
      // A wake-up that doesn't open now stays a placeholder; the next change looks again.
      runCatching { opened.notificationFromStore(payload, offsetMinutes) }
        .onSuccess { handler.handleRetry(it, PushPath.FILLED_STORE) }
    }
  }

  private companion object {
    const val DEBOUNCE_MS = 500L
    const val MS_PER_MINUTE = 60_000
  }
}
