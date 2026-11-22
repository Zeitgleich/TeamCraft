package de.zeitgleich.teamcraft.calls

import android.content.res.Resources
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.size
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import kotlinx.coroutines.delay

/** Joins and leaves of one window are said together after this long (#2281). */
internal const val ANNOUNCE_WINDOW_MS = 1500L

/** Up to this many names are spoken; more are counted. */
internal const val ANNOUNCE_MAX_NAMED = 3

/** Who came and who went between two looks at the call's people, by user ID (the names may be empty). */
internal class RosterChange(val joined: List<String>, val left: List<String>) {
  val isEmpty: Boolean get() = joined.isEmpty() && left.isEmpty()
}

/**
 * The difference of two rosters, in a stable order. A person in both is no news; a join and a
 * leave of the same person inside one window cancel out in [RosterChange.plus].
 */
internal fun rosterChange(before: Collection<String>, now: Collection<String>): RosterChange =
  RosterChange(now.filter { it !in before }.sorted(), before.filter { it !in now }.sorted())

internal operator fun RosterChange.plus(other: RosterChange): RosterChange {
  val cancelled = joined.intersect(other.left.toSet()) + left.intersect(other.joined.toSet())
  return RosterChange(
    (joined + other.joined).filter { it !in cancelled }.distinct(),
    (left + other.left).filter { it !in cancelled }.distinct(),
  )
}

/**
 * Says the call's roster to TalkBack: one live region, in the tree before its first text, fed from
 * the people in the call. Bursts are coalesced ("5 people joined"). The first look is the
 * baseline, not news.
 */
@Composable
internal fun CallRosterAnnouncer(participantIds: Collection<String>, names: Map<String, String>) {
  val resources = LocalContext.current.resources
  var known by remember { mutableStateOf<Set<String>?>(null) }
  var pending by remember { mutableStateOf(RosterChange(emptyList(), emptyList())) }
  var message by remember { mutableStateOf("") }
  var flip by remember { mutableStateOf(false) }
  val currentNames by rememberUpdatedState(names)
  val ids = participantIds.toSet()
  // Each change waits one window for more; the effect of a newer change takes over the pending
  // ones, so a burst becomes one sentence.
  LaunchedEffect(ids) {
    val before = known
    known = ids
    if (before == null) return@LaunchedEffect
    pending += rosterChange(before, ids)
    if (pending.isEmpty) return@LaunchedEffect
    delay(ANNOUNCE_WINDOW_MS)
    val said = pending
    pending = RosterChange(emptyList(), emptyList())
    val text = listOfNotNull(
      sayPeople(resources, said.joined, currentNames, R.string.call_announce_joined, R.plurals.call_announce_joined_many),
      sayPeople(resources, said.left, currentNames, R.string.call_announce_left, R.plurals.call_announce_left_many),
    ).joinToString(". ")
    if (text.isEmpty()) return@LaunchedEffect
    // The same words twice in a row still change the region.
    flip = !flip
    message = if (flip) "$text\u200b" else text
  }
  Box(
    Modifier.size(1.dp).semantics {
      liveRegion = LiveRegionMode.Polite
      contentDescription = message
    },
  )
}

private fun sayPeople(resources: Resources, ids: List<String>, names: Map<String, String>, named: Int, many: Int): String? {
  if (ids.isEmpty()) return null
  if (ids.size > ANNOUNCE_MAX_NAMED) return resources.getQuantityString(many, ids.size, ids.size)
  val someone = resources.getString(R.string.call_recording_someone)
  return resources.getString(named, ids.joinToString(", ") { names[it]?.takeIf { name -> name.isNotEmpty() } ?: someone })
}
