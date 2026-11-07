package de.zeitgleich.teamcraft.ui.screens

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.ChatClient
import de.zeitgleich.teamcraft.ui.ListPhase
import de.zeitgleich.teamcraft.ui.MainViewModel
import de.zeitgleich.teamcraft.ui.Read
import de.zeitgleich.teamcraft.ui.after
import de.zeitgleich.teamcraft.ui.listPhase
import de.zeitgleich.teamcraft.ui.map
import uniffi.teamcraft_ffi.FirstSyncStageItem
import de.zeitgleich.teamcraft.ui.failureText

private const val SKELETON_ROWS = 6

/**
 * Placeholder rows for a list that has no answer yet (issue #297). Static on purpose: calm, and
 * nothing to switch off for reduced motion. Announced once as "Loading" instead of row by row.
 */
@Composable
fun ListSkeleton(modifier: Modifier = Modifier, rows: Int = SKELETON_ROWS) {
  val loading = stringResource(R.string.list_loading)
  Column(modifier.fillMaxWidth().semantics { contentDescription = loading }) {
    repeat(rows) { index ->
      Column(Modifier.padding(horizontal = 16.dp, vertical = 12.dp)) {
        SkeletonBar(0.35f, 12.dp)
        Box(Modifier.height(8.dp))
        SkeletonBar(if (index % 2 == 0) 0.9f else 0.7f, 14.dp)
      }
    }
  }
}

@Composable
private fun SkeletonBar(fraction: Float, height: androidx.compose.ui.unit.Dp) {
  Box(
    Modifier
      .fillMaxWidth(fraction)
      .height(height)
      .clip(RoundedCornerShape(height / 2))
      .background(MaterialTheme.colorScheme.surfaceVariant),
  )
}

/** Why a list could not be read, with the way to ask again. */
@Composable
fun ListFailed(phase: ListPhase.Failed, onRetry: () -> Unit, modifier: Modifier = Modifier) {
  Column(
    modifier.fillMaxWidth().padding(horizontal = 32.dp, vertical = 48.dp),
    horizontalAlignment = Alignment.CenterHorizontally,
  ) {
    Text(
      stringResource(failureText(phase.failure)),
      style = MaterialTheme.typography.bodyMedium,
      color = MaterialTheme.colorScheme.onSurfaceVariant,
      textAlign = TextAlign.Center,
      modifier = Modifier.width(280.dp),
    )
    TextButton(onClick = onRetry) { Text(stringResource(R.string.retry)) }
  }
}

/** One lazy-list item for a phase that is not [ListPhase.Rows]: placeholders, the failure, or [empty]. */
fun LazyListScope.placeholderItem(phase: ListPhase, onRetry: () -> Unit, empty: @Composable () -> Unit) {
  when (phase) {
    ListPhase.Rows -> Unit
    ListPhase.Skeleton -> item(key = "placeholder") { ListSkeleton() }
    ListPhase.Empty -> item(key = "placeholder") { empty() }
    is ListPhase.Failed -> item(key = "placeholder") { ListFailed(phase, onRetry) }
  }
}

/** The same outside a lazy list. */
@Composable
fun ListPlaceholder(phase: ListPhase, onRetry: () -> Unit, empty: @Composable () -> Unit) {
  when (phase) {
    ListPhase.Rows -> Unit
    ListPhase.Skeleton -> ListSkeleton()
    ListPhase.Empty -> empty()
    is ListPhase.Failed -> ListFailed(phase, onRetry)
  }
}

/** A store read that is kept fresh, and how to ask for it again. */
class ReadState<T>(
  val read: Read<T>,
  /** The user pressed "Try again": back to placeholders until the read answers. */
  val retry: () -> Unit,
  /** Reads again, keeping what shows. */
  val refresh: () -> Unit,
) {
  val maybeValue: T? get() = (read as? Read.Loaded)?.value
}

/** The phase of a list read as [ReadState], for a screen whose list is [items] of its value. */
fun <T, R> ReadState<T>.phase(isSyncing: Boolean = false, items: (T) -> List<R>): ListPhase =
  listPhase(read.map(items), isSyncing)

/**
 * Reads from the store whenever it changes (and when [key] does). A read that fails shows rows
 * already read, or a failure with a retry; it never reads as empty.
 */
@Composable
fun <T> rememberRead(
  viewModel: MainViewModel,
  key: Any? = Unit,
  enabled: Boolean = true,
  load: suspend ChatClient.() -> T?,
): ReadState<T> {
  val changes by viewModel.changeCount.collectAsState()
  var read by remember(key) { mutableStateOf<Read<T>>(Read.Loading) }
  var attempt by remember(key) { mutableIntStateOf(0) }
  val currentLoad by rememberUpdatedState(load)
  LaunchedEffect(changes, key, attempt, enabled) {
    if (enabled) read = read.after(viewModel.readChat { currentLoad() })
  }
  return ReadState(
    read = read,
    retry = {
      if (read is Read.Failed) read = Read.Loading
      attempt += 1
    },
    refresh = { attempt += 1 },
  )
}

/** The first sync is still bringing the newest messages, so lists built from them aren't complete. */
@Composable
fun rememberNewestSyncing(viewModel: MainViewModel): Boolean = rememberSyncStage(viewModel) { it == FirstSyncStageItem.NEWEST }

/** Any first sync is running: search results and the like may still be incomplete. */
@Composable
fun rememberHistorySyncing(viewModel: MainViewModel): Boolean = rememberSyncStage(viewModel) { true }

@Composable
private fun rememberSyncStage(viewModel: MainViewModel, matches: (FirstSyncStageItem) -> Boolean): Boolean {
  val changes by viewModel.changeCount.collectAsState()
  var isSyncing by remember { mutableStateOf(false) }
  LaunchedEffect(changes) { isSyncing = viewModel.firstSync()?.stage?.let(matches) == true }
  return isSyncing
}
