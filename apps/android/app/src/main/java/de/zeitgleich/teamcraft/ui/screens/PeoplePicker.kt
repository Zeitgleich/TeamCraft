package de.zeitgleich.teamcraft.ui.screens

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.CheckCircle
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.User
import de.zeitgleich.teamcraft.ui.UserAvatar
import de.zeitgleich.teamcraft.ui.avatarColorOfHue
import de.zeitgleich.teamcraft.ui.theme.Spacing
import kotlinx.coroutines.delay

/** How long the picker waits after a keystroke before it asks the directory. */
private const val SEARCH_PAUSE_MS = 250L

/** The most people one search lists; typing narrows it. */
internal const val PEOPLE_SHOWN = 100

/** How the picker takes a tap. */
internal enum class PickMode {
  /** Several people, each with a check (a group, a channel, a call). */
  Several,

  /** One person; the tap is the choice (an exception to do not disturb). */
  One,
}

/**
 * The one people picker of the app (#1534): a search over the directory through the core
 * (`ChatClient.searchPeople`), the same everywhere people are picked. [search] runs again on
 * [changes], since the server's directory answers as a change. People in [excluded] aren't
 * offered; picked people stay on top while the search changes.
 */
@Composable
internal fun PeoplePicker(
  search: suspend (String) -> List<User>?,
  changes: Long,
  picked: List<String>,
  onPick: (User) -> Unit,
  modifier: Modifier = Modifier,
  excluded: Set<String> = emptySet(),
  mode: PickMode = PickMode.Several,
  isEnabled: Boolean = true,
) {
  var query by rememberSaveable { mutableStateOf("") }
  var maybeFound by remember { mutableStateOf<List<User>?>(null) }
  // Who was picked, so a pick stays named when the search no longer finds them.
  var known by remember { mutableStateOf(mapOf<String, User>()) }
  LaunchedEffect(changes, query) {
    if (query.isNotEmpty()) delay(SEARCH_PAUSE_MS)
    search(query.trim())?.let { found -> maybeFound = found }
  }
  val found = maybeFound.orEmpty().filter { it.id !in excluded }
  val shown = picked.mapNotNull { id -> found.firstOrNull { it.id == id } ?: known[id] } + found.filter { it.id !in picked }
  Column(modifier) {
    CompactSearchField(
      value = query,
      onChange = { query = it },
      placeholder = stringResource(R.string.browse_filter_people),
      modifier = Modifier.padding(horizontal = Spacing.l, vertical = Spacing.s),
    )
    if (maybeFound != null && shown.isEmpty()) {
      Text(
        stringResource(R.string.browse_no_people),
        style = MaterialTheme.typography.bodyMedium,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.fillMaxWidth().padding(Spacing.xxl),
      )
    }
    LazyColumn(Modifier.fillMaxWidth()) {
      items(shown, key = { it.id }) { user ->
        val pick = {
          known = known + (user.id to user)
          onPick(user)
        }
        val rowModifier = when (mode) {
          PickMode.Several -> Modifier.toggleable(value = user.id in picked, role = Role.Checkbox, enabled = isEnabled) { pick() }
          PickMode.One -> Modifier.clickable(enabled = isEnabled, role = Role.Button) { pick() }
        }
        PersonRow(user, isPicked = user.id in picked, mode = mode, modifier = rowModifier)
      }
    }
  }
}

@Composable
private fun PersonRow(user: User, isPicked: Boolean, mode: PickMode, modifier: Modifier) {
  Row(
    modifier.fillMaxWidth().padding(horizontal = Spacing.l, vertical = Spacing.s),
    verticalAlignment = Alignment.CenterVertically,
  ) {
    Box {
      UserAvatar(user.id, Modifier.size(40.dp)) {
        Box(Modifier.size(40.dp).background(avatarColorOfHue(user.avatarHue), CircleShape), contentAlignment = Alignment.Center) {
          Text(user.displayName.firstOrNull()?.uppercase().orEmpty(), color = Color.White, style = MaterialTheme.typography.titleMedium)
        }
      }
      Box(
        Modifier.align(Alignment.BottomEnd).size(14.dp).background(MaterialTheme.colorScheme.background, CircleShape),
        contentAlignment = Alignment.Center,
      ) { PresenceDot(user.presence) }
    }
    Column(Modifier.weight(1f).padding(start = Spacing.m)) {
      Text(user.displayName, maxLines = 1, overflow = TextOverflow.Ellipsis)
      Text(
        listOfNotNull("@" + user.username, user.maybeCustomStatus?.let { it.emoji + " " + it.text }).joinToString(" · "),
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        maxLines = 1,
        overflow = TextOverflow.Ellipsis,
      )
    }
    // Picked people get a filled check, the others an empty ring, so it reads as a choice.
    if (mode == PickMode.Several) {
      if (isPicked) {
        Icon(Icons.Filled.CheckCircle, contentDescription = null, tint = MaterialTheme.colorScheme.primary)
      } else {
        Box(Modifier.padding(2.dp).size(20.dp).border(2.dp, MaterialTheme.colorScheme.outline, CircleShape))
      }
    }
  }
}
