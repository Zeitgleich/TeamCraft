package de.zeitgleich.teamcraft.ui.screens

import androidx.compose.foundation.layout.consumeWindowInsets
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.ArrowDropDown
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.Search
import androidx.compose.material3.Badge
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.Text
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.ChatActions
import de.zeitgleich.teamcraft.core.ChatQueries
import de.zeitgleich.teamcraft.core.JoinableSpace
import de.zeitgleich.teamcraft.ui.HomeSpace
import de.zeitgleich.teamcraft.ui.MainViewModel
import de.zeitgleich.teamcraft.ui.SelectedSpace
import de.zeitgleich.teamcraft.ui.SpacePicture
import de.zeitgleich.teamcraft.ui.theme.Spacing

/**
 * A space's picture, or the first letter of its name on a tile when it has none, so every space in
 * the switcher has a mark of its own (#1526).
 */
@Composable
fun SpaceTile(spaceId: String, name: String, maybeImageVersion: String?, size: Dp = 32.dp) {
  if (maybeImageVersion != null) {
    SpacePicture(spaceId, maybeImageVersion, size = size)
    return
  }
  Box(
    Modifier.size(size).clip(RoundedCornerShape(size / 4)).background(MaterialTheme.colorScheme.secondaryContainer),
    contentAlignment = Alignment.Center,
  ) {
    Text(
      name.trim().take(1).uppercase(),
      style = if (size >= 32.dp) MaterialTheme.typography.titleSmall else MaterialTheme.typography.labelMedium,
      color = MaterialTheme.colorScheme.onSecondaryContainer,
    )
  }
}

/**
 * Home's title in the spaces layout (#1526): the selected space with a drop-down mark; a tap opens
 * the switcher. A dot on the mark says another space has something new, so switching hides nothing.
 */
@Composable
internal fun SpaceSwitcherTitle(space: HomeSpace, hasNewElsewhere: Boolean, onClick: () -> Unit) {
  val label = stringResource(R.string.space_switcher_open, space.name)
  val newElsewhere = stringResource(R.string.space_switcher_new_elsewhere)
  Row(
    Modifier
      .clip(RoundedCornerShape(12.dp))
      .clickable(role = Role.Button, onClick = onClick)
      .semantics(mergeDescendants = true) {
        heading()
        contentDescription = label
        if (hasNewElsewhere) stateDescription = newElsewhere
      }
      .padding(horizontal = Spacing.xs, vertical = Spacing.xs),
    verticalAlignment = Alignment.CenterVertically,
    horizontalArrangement = Arrangement.spacedBy(Spacing.s),
  ) {
    SpaceTile(space.id, space.name, space.maybeImageVersion, size = 28.dp)
    Text(
      space.name,
      style = MaterialTheme.typography.titleLarge,
      maxLines = 1,
      overflow = TextOverflow.Ellipsis,
      modifier = Modifier.weight(1f, fill = false),
    )
    Box {
      Icon(Icons.Filled.ArrowDropDown, contentDescription = null)
      if (hasNewElsewhere) Badge(Modifier.align(Alignment.TopEnd).size(8.dp))
    }
  }
}

/**
 * Every space the user belongs to (#1526): its mark, its name and what is new in it; a tap selects
 * it. "Browse spaces" follows where the user may join spaces (not guests), "New space" where the
 * workspace role allows creating one (#1534).
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun SpaceSwitcherSheet(
  spaces: List<HomeSpace>,
  maybeSelectedId: String?,
  canBrowse: Boolean,
  onSelect: (String) -> Unit,
  onBrowse: () -> Unit,
  onDismiss: () -> Unit,
  canCreate: Boolean = false,
  onCreate: () -> Unit = {},
) {
  val sheet = rememberModalBottomSheetState(skipPartiallyExpanded = spaces.size > 6)
  ModalBottomSheet(onDismissRequest = onDismiss, sheetState = sheet) {
    Text(
      stringResource(R.string.nav_spaces),
      style = MaterialTheme.typography.titleMedium,
      modifier = Modifier.padding(horizontal = Spacing.l, vertical = Spacing.s).semantics { heading() },
    )
    LazyColumn(Modifier.fillMaxWidth().navigationBarsPadding()) {
      if (spaces.isEmpty()) item(key = "none") { EmptyHint(stringResource(R.string.spaces_none)) }
      items(spaces, key = { it.id }) { space ->
        SpaceSwitcherRow(space, isSelected = space.id == maybeSelectedId) { onSelect(space.id) }
      }
      if (canBrowse || canCreate) item(key = "divider") { HorizontalDivider(Modifier.padding(vertical = Spacing.xs)) }
      if (canCreate) {
        item(key = "create") {
          ListItem(
            headlineContent = { Text(stringResource(R.string.create_space)) },
            leadingContent = { Icon(Icons.Filled.Add, contentDescription = null, tint = MaterialTheme.colorScheme.primary) },
            colors = ListItemDefaults.colors(containerColor = MaterialTheme.colorScheme.surfaceContainerLow),
            modifier = Modifier.clickable(role = Role.Button, onClick = onCreate),
          )
        }
      }
      if (canBrowse) {
        item(key = "browse") {
          ListItem(
            headlineContent = { Text(stringResource(R.string.spaces_browse)) },
            leadingContent = { Icon(Icons.Filled.Search, contentDescription = null, tint = MaterialTheme.colorScheme.primary) },
            colors = ListItemDefaults.colors(containerColor = MaterialTheme.colorScheme.surfaceContainerLow),
            modifier = Modifier.clickable(role = Role.Button, onClick = onBrowse),
          )
        }
      }
    }
  }
}

@Composable
private fun SpaceSwitcherRow(space: HomeSpace, isSelected: Boolean, onClick: () -> Unit) {
  val selected = stringResource(R.string.space_switcher_selected)
  ListItem(
    headlineContent = { Text(space.name, maxLines = 1, overflow = TextOverflow.Ellipsis) },
    leadingContent = { SpaceTile(space.id, space.name, space.maybeImageVersion) },
    trailingContent = {
      Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(Spacing.s)) {
        SpaceBadge(space)
        if (isSelected) Icon(Icons.Filled.Check, contentDescription = null, tint = MaterialTheme.colorScheme.primary)
      }
    },
    colors = ListItemDefaults.colors(
      containerColor = if (isSelected) MaterialTheme.colorScheme.secondaryContainer else MaterialTheme.colorScheme.surfaceContainerLow,
    ),
    modifier = Modifier
      .padding(horizontal = Spacing.s)
      .clip(RoundedCornerShape(16.dp))
      .clickable(role = Role.Button, onClick = onClick)
      .semantics { if (isSelected) stateDescription = selected },
  )
}

/** Mentions as a number, other news as a dot, as the bottom navigation counts channels. */
@Composable
internal fun SpaceBadge(space: HomeSpace) {
  when {
    space.mentionCount > 0 -> {
      val text = pluralStringResource(R.plurals.chat_mentions, space.mentionCount, space.mentionCount)
      Badge(Modifier.semantics { contentDescription = text }) { Text(space.mentionCount.toString()) }
    }
    space.hasUnread -> {
      val text = stringResource(R.string.space_badge_new)
      Box(Modifier.size(8.dp).clip(CircleShape).background(MaterialTheme.colorScheme.primary).semantics { contentDescription = text })
    }
  }
}

/**
 * The open spaces the user isn't in, from the space list the device holds (#1526). Joining one
 * selects it on Home once the core has it, and goes back there.
 */
@Composable
fun BrowseSpacesScreen(viewModel: MainViewModel, onBack: () -> Unit) {
  val changes by viewModel.changeCount.collectAsState()
  val context = LocalContext.current
  val snackbars = rememberFailureSnackbars(viewModel)
  var maybeSpaces by remember { mutableStateOf<List<JoinableSpace>?>(null) }
  var maybeJoining by rememberSaveable { mutableStateOf<String?>(null) }
  LaunchedEffect(changes) {
    maybeSpaces = viewModel.readChat { query(ChatQueries.joinableSpaces()).spaces } ?: maybeSpaces ?: emptyList()
    val joining = maybeJoining ?: return@LaunchedEffect
    if (viewModel.readChat { snapshot() }?.spaces?.any { it.id == joining } == true) {
      SelectedSpace.of(context).select(joining)
      maybeJoining = null
      onBack()
    }
  }
  Scaffold(
    contentWindowInsets = ScreenInsets,
    topBar = { ChromeTopBar(title = stringResource(R.string.spaces_browse), maybeOnBack = onBack) },
    snackbarHost = { SnackbarHost(snackbars) },
  ) { padding ->
    val spaces = maybeSpaces
    Column(Modifier.fillMaxSize().padding(padding.withoutBottom()).consumeWindowInsets(padding)) {
      when {
        spaces == null -> LoadingScreen()
        spaces.isEmpty() -> EmptyHint(stringResource(R.string.spaces_browse_empty))
        else -> LazyColumn(Modifier.fillMaxSize(), contentPadding = padding.bottomOnly()) {
          items(spaces, key = { it.id }) { space ->
            JoinableSpaceRow(space, isJoining = maybeJoining == space.id) {
              maybeJoining = space.id
              viewModel.act(ChatActions.joinSpace(space.id), onRefused = { maybeJoining = null })
            }
          }
        }
      }
    }
  }
}

@Composable
private fun JoinableSpaceRow(space: JoinableSpace, isJoining: Boolean, onJoin: () -> Unit) {
  val joinLabel = stringResource(R.string.space_join_named, space.name)
  ListItem(
    headlineContent = { Text(space.name, maxLines = 1, overflow = TextOverflow.Ellipsis) },
    supportingContent = {
      Text(
        listOfNotNull(
          space.maybeDescription?.takeIf { it.isNotBlank() },
          pluralStringResource(R.plurals.space_member_count, space.memberCount, space.memberCount),
        ).joinToString(" · "),
        maxLines = 2,
        overflow = TextOverflow.Ellipsis,
      )
    },
    leadingContent = { SpaceTile(space.id, space.name, space.maybeImageVersion, size = 40.dp) },
    trailingContent = {
      FilledTonalButton(
        onClick = onJoin,
        enabled = !isJoining,
        modifier = Modifier.semantics { contentDescription = joinLabel },
      ) { Text(stringResource(R.string.space_join)) }
    },
  )
}
