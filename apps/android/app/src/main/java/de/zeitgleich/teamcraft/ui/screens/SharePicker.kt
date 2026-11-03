package de.zeitgleich.teamcraft.ui.screens

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
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
import androidx.compose.material3.Text
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
import de.zeitgleich.teamcraft.ui.SharedContent
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
fun SharePickerScreen(viewModel: MainViewModel, content: SharedContent, onPick: (String) -> Unit, onBack: () -> Unit) {
  val context = LocalContext.current
  val notesTitle = stringResource(R.string.notes_title)
  var state by remember { mutableStateOf<ShareTargets>(ShareTargets.Loading) }
  var attempt by remember { mutableIntStateOf(0) }
  var filter by rememberSaveable { mutableStateOf("") }
  // Once per visit: the order rarely changes while the picker is open.
  var hasPublished by remember { mutableStateOf(false) }
  val pick: (String) -> Unit = { conversation ->
    ShareShortcuts.reportUsed(context, conversation)
    onPick(conversation)
  }
  LaunchedEffect(attempt) {
    // Read again on changes: the notes appear once the first catch-up made them.
    viewModel.changeCount.collect {
      val maybeTargets = viewModel.shareTargets()
      if (maybeTargets == null) {
        if (state !is ShareTargets.Loaded) state = ShareTargets.Failed
        return@collect
      }
      content.maybeTarget?.takeIf { target -> maybeTargets.any { it.id == target } }?.let { target ->
        pick(target)
        return@collect
      }
      state = ShareTargets.Loaded(maybeTargets)
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
        maybeSubtitle = shareSummary(content),
        maybeOnBack = onBack,
      )
    },
  ) { padding ->
    Column(Modifier.fillMaxSize().padding(padding)) {
      when (val current = state) {
        ShareTargets.Loading -> LoadingScreen()
        ShareTargets.Failed -> EmptyState(
          icon = Icons.Filled.Refresh,
          title = stringResource(R.string.share_load_failed_title),
          text = stringResource(R.string.share_load_failed),
          maybeAction = stringResource(R.string.retry) to {
            state = ShareTargets.Loading
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
private fun shareSummary(content: SharedContent): String = when {
  content.uris.isNotEmpty() -> pluralStringResource(R.plurals.share_files, content.uris.size, content.uris.size)
  else -> content.text
}
