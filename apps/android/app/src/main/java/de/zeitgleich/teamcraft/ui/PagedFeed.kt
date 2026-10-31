package de.zeitgleich.teamcraft.ui

import de.zeitgleich.teamcraft.core.Message
import de.zeitgleich.teamcraft.core.MessageTail
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

/**
 * The part of a conversation a feed shows while the reader pages back (issue #1058). The core
 * reads the newest messages down to the oldest one shown, so a reread after a change keeps the
 * rows paged to and new messages don't push old ones out; [older] adds the next older page. Reads
 * run one at a time: a reread that started before a page back can't drop that page again.
 */
class PagedFeed(
  private val firstPage: Int,
  private val pageBack: Int,
  private val read: suspend (maybeOldest: Message?, older: Int) -> MessageTail?,
) {
  private val mutex = Mutex()
  private var maybeOldest: Message? = null

  /** The messages shown so far, read again (the newest [firstPage] before the first read). */
  suspend fun reread(): MessageTail? = mutex.withLock { load(if (maybeOldest == null) firstPage else 0) }

  /** The messages shown so far and the next older page. */
  suspend fun older(): MessageTail? = mutex.withLock { load(pageBack) }

  private suspend fun load(older: Int): MessageTail? {
    val tail = read(maybeOldest, older) ?: return null
    maybeOldest = tail.messages.firstOrNull()
    return tail
  }
}
