package de.zeitgleich.teamcraft.notifications

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.FlowPreview
import kotlinx.coroutines.flow.debounce
import kotlinx.coroutines.flow.drop
import kotlinx.coroutines.launch
import de.zeitgleich.teamcraft.core.CoreRepository

/**
 * While the process lives, removes a conversation's notification as soon as the core reports it
 * read (on another device, synced through the gateway), without waiting for the `dismiss` push.
 */
class ReadWatcher(
  private val core: CoreRepository,
  private val notifier: MessageNotifier,
  private val scope: CoroutineScope,
) {
  /** Unread counts last seen, per conversation. */
  private var unread: Map<String, Int> = emptyMap()

  @OptIn(FlowPreview::class)
  fun start() {
    scope.launch {
      core.changeCount.drop(1).debounce(DEBOUNCE_MS).collect { check() }
    }
  }

  private suspend fun check() {
    val shown = notifier.shownConversationIds()
    val conversations = runCatching { core.core().conversations() }.getOrNull() ?: return
    val current = conversations.associate { it.id to (it.unreadCount + it.mentionCount).toInt() }
    ReadTransitions.becameRead(unread, current, shown).forEach(notifier::dismiss)
    unread = current
  }

  private companion object {
    const val DEBOUNCE_MS = 500L
  }
}

/** Pure, unit-tested: which shown conversations were just read. */
object ReadTransitions {
  /**
   * Conversations with a notification that had unread messages before and have none now. A
   * conversation never seen unread is left alone: its store may not have the pushed message yet.
   */
  fun becameRead(previous: Map<String, Int>, current: Map<String, Int>, shown: Set<String>): Set<String> =
    shown.filterTo(mutableSetOf()) { id -> (previous[id] ?: 0) > 0 && current[id] == 0 }
}
