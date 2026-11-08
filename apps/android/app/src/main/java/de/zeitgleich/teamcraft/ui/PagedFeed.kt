package de.zeitgleich.teamcraft.ui

import de.zeitgleich.teamcraft.core.HistoryGap
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

/** What the core says of the history beyond what this device holds (`OlderHistory` in Rust). */
object ServerHistory {
  const val AVAILABLE = "available"
  const val LOADING = "loading"
  const val FAILED = "failed"
  const val COMPLETE = "complete"
}

/** What the top of a feed offers: the rows the store holds come first, then the server's. */
data class OlderState(val hasOlder: Boolean, val isLoading: Boolean, val isFailed: Boolean)

fun olderState(hasLocalOlder: Boolean, isReading: Boolean, serverHistory: String): OlderState = OlderState(
  hasOlder = hasLocalOlder || serverHistory == ServerHistory.AVAILABLE || serverHistory == ServerHistory.LOADING,
  isLoading = isReading || (!hasLocalOlder && serverHistory == ServerHistory.LOADING),
  isFailed = !hasLocalOlder && serverHistory == ServerHistory.FAILED,
)

/** Which side of a hole in the history a page is read from (`GapEdge` in Rust). */
object GapEdge {
  /** Its oldest messages, after the row before it: the reader reads down towards it. */
  const val OLDER = "older"

  /** Its newest messages, before the row after it: the reader reads up towards it. */
  const val NEWER = "newer"
}

/** A page of [gap] to ask the core for, read from [edge]. */
data class GapRequest(val gap: HistoryGap, val edge: String)

/**
 * The side of a hole at feed row [row] the reader comes from, with [topVisible] the topmost row on
 * screen (rows counted from the newest, as the reversed list holds them): from below the topmost
 * row the reader reads down towards it, so what arrives continues what they look at.
 */
fun gapEdge(row: Int, topVisible: Int): String = if (row < topVisible) GapEdge.OLDER else GapEdge.NEWER

/**
 * The hole a feed asks a page of (issue #1438, as web and desktop do): the first, oldest first, whose
 * row lies within [ahead] rows of those on screen ([bottomVisible] to [topVisible]); none while the
 * conversation's last request is on its way or failed (that waits for "Try again" or the next
 * connection). [gapRows] are the holes with their rows, counted from the newest.
 */
fun gapToFill(gapRows: List<Pair<Int, HistoryGap>>, bottomVisible: Int, topVisible: Int, ahead: Int): GapRequest? {
  val (row, gap) = gapRows
    .sortedByDescending { it.first }
    .firstOrNull { (candidate, _) -> candidate in (bottomVisible - ahead)..(topVisible + ahead) } ?: return null
  if (gap.status != ServerHistory.AVAILABLE) return null
  return GapRequest(gap, gapEdge(row, topVisible))
}
