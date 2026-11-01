package de.zeitgleich.teamcraft.ui

import java.time.LocalDate

/** A day divider on screen: its day and where its pill's top edge is, in pixels from the feed's top. */
data class DividerOnScreen(val date: LocalDate, val pillTop: Int)

/** The day pinned over the feed and how far the next day's divider pushes it up (negative pixels). */
data class PinnedDay(val date: LocalDate, val push: Int)

/**
 * The day pinned at the top of the feed while it scrolls (#1046), like the section headers of iOS.
 * A day becomes the pinned one once its divider's pill has reached the pinned pill's place
 * ([pinTop]), so the two never show side by side; the divider below keeps a gap of [pinTop] while
 * it pushes the old pill out. [topmostDay] is the day of the topmost row on screen, the day
 * carried in from above when no divider has reached the top yet; `null` above the first day.
 */
fun pinnedDay(dividers: List<DividerOnScreen>, topmostDay: LocalDate?, pinTop: Int, pillHeight: Int): PinnedDay? {
  val maybeReached = dividers.filter { it.pillTop <= pinTop }.maxByOrNull { it.pillTop }
  val date = maybeReached?.date ?: topmostDay ?: return null
  val push = dividers.filter { it.pillTop > pinTop }.minOfOrNull { it.pillTop }
    ?.let { minOf(0, it - 2 * pinTop - pillHeight) } ?: 0
  return PinnedDay(date, push)
}
