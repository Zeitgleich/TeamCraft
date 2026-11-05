package de.zeitgleich.teamcraft.ui.screens

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Email
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material.icons.filled.Search
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.ui.MainViewModel
import de.zeitgleich.teamcraft.ui.SectionTitle
import de.zeitgleich.teamcraft.ui.ShareShortcuts
import de.zeitgleich.teamcraft.ui.ShareState
import de.zeitgleich.teamcraft.ui.StagedShare
import de.zeitgleich.teamcraft.ui.listName
import de.zeitgleich.teamcraft.ui.shareSections
import de.zeitgleich.teamcraft.ui.theme.Spacing
import uniffi.teamcraft_ffi.ConversationItem

/** What the share picker has to show: still loading, the targets, or a read that failed. */
private sealed interface ShareTargets {
  data object Loading : ShareTargets
  data class Loaded(val targets: List<ConversationItem>) : ShareTargets
  data object Failed : ShareTargets
}

/**
 * Where to share what another app sent (issue #829): the notes on top, the recent conversations,
 * then the DMs and the channels by space, as in the chat list, filterable. The pick opens the
 * conversation with the composer filled in; nothing is sent without the user, so an offline
 * share waits in the composer and goes through its send queue. A direct share shortcut picks the
 * conversation without showing the list.
 */
@Composable
fun SharePickerScreen(
  viewModel: MainViewModel,
  state: ShareState,
  onPick: (StagedShare, String) -> Unit,
  onBack: () -> Unit,
) {
  val maybeContent = (state as? ShareState.Ready)?.share
  val context = LocalContext.current
  val notesTitle = stringResource(R.string.notes_title)
  var targets by remember { mutableStateOf<ShareTargets>(ShareTargets.Loading) }
  var attempt by remember { mutableIntStateOf(0) }
  var filter by rememberSaveable { mutableStateOf("") }
  // Once per visit: the order rarely changes while the picker is open.
  var hasPublished by remember { mutableStateOf(false) }
  val pick: (String) -> Unit = { conversation ->
    ShareShortcuts.reportUsed(context, conversation)
    maybeContent?.let { onPick(it, conversation) }
  }
  LaunchedEffect(attempt, maybeContent?.id) {
    // Read again on changes: the notes appear once the first catch-up made them.
    viewModel.changeCount.collect {
      val maybeTargets = viewModel.shareTargets()
      if (maybeTargets == null) {
        if (targets !is ShareTargets.Loaded) targets = ShareTargets.Failed
        return@collect
      }
      maybeContent?.maybeTarget?.takeIf { target -> maybeTargets.any { it.id == target } }?.let { target ->
        pick(target)
        return@collect
      }
      targets = ShareTargets.Loaded(maybeTargets)
      if (!hasPublished) {
        hasPublished = true
        ShareShortcuts.publish(context, shortcutTargets(maybeTargets, notesTitle))
      }
    }
  }
  Scaffold(
    contentWindowInsets = ScreenInsets,
    topBar = {
      ChromeTopBar(
        title = stringResource(R.string.share_title),
        maybeSubtitle = maybeContent?.let { shareSummary(it) },
        maybeOnBack = onBack,
      )
    },
  ) { padding ->
    Column(Modifier.fillMaxSize().padding(padding)) {
      if (maybeContent != null && maybeContent.leftOut > 0) {
        Text(
          pluralStringResource(R.plurals.share_left_out, maybeContent.leftOut, maybeContent.leftOut),
          modifier = Modifier.padding(horizontal = Spacing.l, vertical = Spacing.s),
          style = MaterialTheme.typography.bodyMedium,
          color = MaterialTheme.colorScheme.error,
        )
      }
      if (maybeContent == null) {
        // The files are still being copied; back cancels.
        LoadingScreen()
        return@Column
      }
      if (maybeContent.isEmpty) {
        EmptyState(
          icon = Icons.Filled.Refresh,
          title = stringResource(R.string.share_nothing_title),
          text = stringResource(R.string.share_nothing),
          maybeAction = stringResource(R.string.back) to onBack,
        )
        return@Column
      }
      when (val current = targets) {
        ShareTargets.Loading -> LoadingScreen()
        ShareTargets.Failed -> EmptyState(
          icon = Icons.Filled.Refresh,
          title = stringResource(R.string.share_load_failed_title),
          text = stringResource(R.string.share_load_failed),
          maybeAction = stringResource(R.string.retry) to {
            targets = ShareTargets.Loading
            attempt += 1
          },
        )
        is ShareTargets.Loaded -> if (current.targets.isEmpty()) {
          EmptyState(
            icon = Icons.Filled.Email,
            title = stringResource(R.string.share_empty_title),
            text = stringResource(R.string.share_empty),
            maybeAction = null,
          )
        } else {
          ShareTargetList(current.targets, notesTitle, filter, onFilter = { filter = it }, onPick = pick)
        }
      }
    }
  }
}

/** The search field and the sections; only the visible rows are composed. */
@Composable
private fun ShareTargetList(
  targets: List<ConversationItem>,
  notesTitle: String,
  filter: String,
  onFilter: (String) -> Unit,
  onPick: (String) -> Unit,
) {
  // Grouping runs over the list in memory, once per keystroke or change, never against the core.
  val sections = remember(targets, notesTitle, filter) { shareSections(targets, notesTitle, filter) }
  LazyColumn(Modifier.fillMaxSize()) {
    item(key = "filter") { ShareFilter(filter, onFilter) }
    if (sections.isEmpty()) item(key = "no-match") { EmptyHint(stringResource(R.string.share_no_match)) }
    for (section in sections) {
      // The notes are their own heading.
      if (section.title != SectionTitle.Notes) item(key = "section-${section.title}") { SectionHeader(section.title) }
      items(section.conversations, key = { "${section.title}-${it.id}" }) { conversation ->
        ShareTargetRow(conversation, conversation.listName(notesTitle), onClick = { onPick(conversation.id) })
      }
    }
  }
}

@Composable
private fun ShareFilter(value: String, onChange: (String) -> Unit) {
  OutlinedTextField(
    value = value,
    onValueChange = onChange,
    modifier = Modifier.fillMaxWidth().padding(horizontal = Spacing.l, vertical = Spacing.s),
    placeholder = { Text(stringResource(R.string.share_filter)) },
    leadingIcon = { Icon(Icons.Filled.Search, contentDescription = null) },
    trailingIcon = {
      if (value.isNotEmpty()) {
        IconButton(onClick = { onChange("") }) {
          Icon(Icons.Filled.Close, contentDescription = stringResource(R.string.clear))
        }
      }
    },
    shape = MaterialTheme.shapes.large,
    singleLine = true,
  )
}

/** A row as in the chat list: the avatar or marker, the name and, for channels, the topic. */
@Composable
private fun ShareTargetRow(conversation: ConversationItem, name: String, onClick: () -> Unit) {
  Row(
    Modifier
      .fillMaxWidth()
      .background(MaterialTheme.colorScheme.background)
      .clickable(onClick = onClick)
      .padding(horizontal = 16.dp, vertical = 10.dp),
    verticalAlignment = Alignment.CenterVertically,
  ) {
    ConversationAvatar(conversation)
    Column(Modifier.weight(1f).padding(start = 12.dp)) {
      Text(name, style = MaterialTheme.typography.titleSmall, maxLines = 1, overflow = TextOverflow.Ellipsis)
      val detail = conversation.topic.ifBlank { conversation.maybeSpaceName.orEmpty() }
      if (detail.isNotBlank()) {
        Text(
          detail,
          style = MaterialTheme.typography.bodyMedium,
          color = MaterialTheme.colorScheme.onSurfaceVariant,
          maxLines = 1,
          overflow = TextOverflow.Ellipsis,
        )
      }
    }
  }
}

/** The notes and the most recent conversations, in the core's order, for the share sheet. */
private fun shortcutTargets(targets: List<ConversationItem>, notesTitle: String): List<ShareShortcuts.Target> =
  targets
    .filter { it.isNotesToSelf || it.maybeLastActivityMs != null }
    .map { ShareShortcuts.Target(it.id, it.listName(notesTitle)) }

@Composable
private fun shareSummary(content: StagedShare): String = when {
  content.files.isNotEmpty() -> pluralStringResource(R.plurals.share_files, content.files.size, content.files.size)
  else -> content.text
}

/**
 * While the sign-in or the app lock comes first: what was shared is kept and comes back after it,
 * and the user can let it go.
 */
@Composable
fun ShareWaitingNotice(onDiscard: () -> Unit, modifier: Modifier = Modifier) {
  Surface(
    modifier = modifier.statusBarsPadding().padding(Spacing.m),
    shape = MaterialTheme.shapes.medium,
    tonalElevation = 3.dp,
    shadowElevation = 3.dp,
  ) {
    Row(
      modifier = Modifier.padding(start = Spacing.l, end = Spacing.s, top = Spacing.xs, bottom = Spacing.xs),
      verticalAlignment = Alignment.CenterVertically,
    ) {
      Text(
        stringResource(R.string.share_waiting),
        modifier = Modifier.weight(1f, fill = false),
        style = MaterialTheme.typography.bodyMedium,
      )
      TextButton(onClick = onDiscard) { Text(stringResource(R.string.share_discard)) }
    }
  }
}
