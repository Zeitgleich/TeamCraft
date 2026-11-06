package de.zeitgleich.teamcraft.ui.screens

import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.heading
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.produceState
import androidx.compose.material3.TextButton
import androidx.compose.foundation.layout.Row
import androidx.compose.ui.Alignment
import androidx.compose.ui.platform.LocalContext
import kotlinx.coroutines.delay
import uniffi.teamcraft_ffi.FirstSyncItem
import uniffi.teamcraft_ffi.FirstSyncStageItem
import uniffi.teamcraft_ffi.NetworkInterception
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.height
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.semantics.contentDescription
import uniffi.teamcraft_ffi.ConnectionState
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.WindowInsetsSides
import androidx.compose.foundation.layout.only
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.TeamCraftApplication
import de.zeitgleich.teamcraft.ui.CONNECTION_GRACE_MS
import de.zeitgleich.teamcraft.ui.ConnectionNotice
import de.zeitgleich.teamcraft.ui.connectionNotice
import de.zeitgleich.teamcraft.ui.isConnectionDown
import de.zeitgleich.teamcraft.ui.rememberIsOnline

/**
 * What content stays clear of (#309): system bars, the display cutout (landscape) and the keyboard.
 * Every screen's Scaffold takes it as `contentWindowInsets`, so nothing hard-codes a padding and
 * the keyboard pushes content up with its own animation.
 */
val ScreenInsets: WindowInsets
  @Composable get() = WindowInsets.safeDrawing

/** Top bars reach behind the status bar but keep their content clear of it and of a side cutout. */
val TopBarInsets: WindowInsets
  @Composable get() = WindowInsets.safeDrawing.only(WindowInsetsSides.Top + WindowInsetsSides.Horizontal)

/** The top bar of the screens behind the main one: back, title and actions on the theme's surface. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ChromeTopBar(
  title: String,
  maybeSubtitle: String? = null,
  maybeOnBack: (() -> Unit)? = null,
  actions: @Composable RowScope.() -> Unit = {},
  maybeIcon: (@Composable () -> Unit)? = null,
) {
  TopAppBar(
    title = {
      androidx.compose.foundation.layout.Row(
        verticalAlignment = androidx.compose.ui.Alignment.CenterVertically,
        horizontalArrangement = androidx.compose.foundation.layout.Arrangement.spacedBy(8.dp),
      ) {
        maybeIcon?.invoke()
        androidx.compose.foundation.layout.Column {
          Text(title, maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.semantics { heading() })
          maybeSubtitle?.takeIf { it.isNotBlank() }?.let {
            Text(
              it,
              maxLines = 1,
              overflow = TextOverflow.Ellipsis,
              style = MaterialTheme.typography.bodySmall,
              color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
          }
        }
      }
    },
    navigationIcon = {
      if (maybeOnBack != null) {
        IconButton(onClick = maybeOnBack) {
          Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = stringResource(R.string.back))
        }
      }
    },
    actions = actions,
    windowInsets = TopBarInsets,
    // The theme's surface, as on the main screen: one header style in light and dark (the status
    // bar icons follow the theme).
    colors = TopAppBarDefaults.topAppBarColors(containerColor = MaterialTheme.colorScheme.surface),
  )
}

/**
 * A thin bar while the app can't reach the server: the device is offline, the server is
 * unreachable although the device is online, or the session expired. Follows the core's connection
 * state; new messages wait, sent ones stay in the outbox. A connection that comes back within the
 * grace period ([CONNECTION_GRACE_MS], from going down or from the app's return) shows nothing: the
 * cached state is on screen and syncs quietly (issue #700).
 */
@Composable
fun OfflineBanner(onSignInAgain: () -> Unit) {
  val graph = TeamCraftApplication.graph(LocalContext.current)
  val isOnline = rememberIsOnline()
  val maybeConnection by graph.core.connectionState.collectAsState()
  val unsettled by graph.connectionGrace.unsettled.collectAsState()
  val isSyncStartFailing by graph.isSyncStartFailing.collectAsState()
  // The clock only matters while the connection is down; a connected app doesn't wake every second.
  val now by produceState(System.currentTimeMillis(), unsettled) {
    value = System.currentTimeMillis()
    while (unsettled.maybeSinceMs != null) {
      delay(COUNTDOWN_MILLIS)
      value = System.currentTimeMillis()
    }
  }
  val notice = connectionNotice(isOnline, maybeConnection, unsettled, now, isSyncStartFailing) ?: return
  val (text, maybeAction) = when (notice) {
    ConnectionNotice.DeviceOffline -> stringResource(R.string.offline_banner) to null
    ConnectionNotice.Connecting -> stringResource(R.string.connection_connecting) to null
    ConnectionNotice.SyncNotStarted -> stringResource(R.string.connection_sync_not_started) to null
    ConnectionNotice.Slow -> stringResource(R.string.connection_slow) to null
    is ConnectionNotice.Unreachable -> {
      val text = when (notice.maybeInterception) {
        NetworkInterception.CAPTIVE_PORTAL -> R.string.connection_captive_portal
        NetworkInterception.UNTRUSTED_CERTIFICATE -> R.string.connection_untrusted_certificate
        null -> R.string.connection_unreachable
      }
      stringResource(text, notice.retryInSeconds.toInt()) to
        (stringResource(R.string.connection_retry) to { graph.core.networkChanged() })
    }
    ConnectionNotice.SignInRequired ->
      stringResource(R.string.connection_sign_in) to (stringResource(R.string.connection_sign_in_action) to onSignInAgain)
    ConnectionNotice.NoRoleLeft ->
      stringResource(R.string.connection_no_role_left) to (stringResource(R.string.connection_sign_in_action) to onSignInAgain)
  }
  // Slow is no failure: the neutral colour, not the error one.
  val isSlow = notice == ConnectionNotice.Slow
  Surface(
    color = if (isSlow) MaterialTheme.colorScheme.secondaryContainer else MaterialTheme.colorScheme.errorContainer,
    modifier = Modifier.fillMaxWidth(),
  ) {
    Row(
      verticalAlignment = Alignment.CenterVertically,
      modifier = Modifier.padding(start = 16.dp, end = 4.dp),
    ) {
      Text(
        text,
        style = MaterialTheme.typography.labelMedium,
        color = if (isSlow) MaterialTheme.colorScheme.onSecondaryContainer else MaterialTheme.colorScheme.onErrorContainer,
        modifier = Modifier.weight(1f).padding(vertical = 6.dp),
      )
      maybeAction?.let { (label, action) ->
        TextButton(onClick = action) { Text(label) }
      }
    }
  }
}

/**
 * A thin moving line under the top bar while the app gets what happened while it was away: the
 * connection is being set up, or [isCatchingUp] (this conversation, or one in the list, still has
 * messages to come). Without it a chat looked complete for seconds and then changed. It shows only
 * after [SYNCING_LINE_DELAY_MILLIS], so a quick catch-up doesn't flash, and keeps its height, so
 * nothing below moves. The banner above speaks once the connection fails for longer.
 */
@Composable
fun SyncingLine(isCatchingUp: Boolean) {
  val graph = TeamCraftApplication.graph(LocalContext.current)
  val isOnline = rememberIsOnline()
  val maybeConnection by graph.core.connectionState.collectAsState()
  val isSyncing = isOnline && (isCatchingUp || maybeConnection is ConnectionState.Connecting)
  val isShown by produceState(false, isSyncing) {
    if (isSyncing) delay(SYNCING_LINE_DELAY_MILLIS)
    value = isSyncing
  }
  val alpha by animateFloatAsState(if (isShown) 1f else 0f, label = "syncing line")
  val description = stringResource(R.string.syncing_updates)
  Box(Modifier.fillMaxWidth().height(SYNCING_LINE_HEIGHT)) {
    if (alpha > 0f) {
      LinearProgressIndicator(
        modifier = Modifier
          .fillMaxSize()
          .graphicsLayer { this.alpha = alpha }
          .semantics { contentDescription = description },
      )
    }
  }
}

/**
 * A first sync's progress under the top bar: what is loading, how far in conversations, and the bar.
 * Numbers from the core; indeterminate until it has one. Gone while the connection is down, where
 * the banner above says why nothing moves.
 */
@Composable
fun FirstSyncBar(progress: FirstSyncItem) {
  val graph = TeamCraftApplication.graph(LocalContext.current)
  val isOnline = rememberIsOnline()
  val maybeConnection by graph.core.connectionState.collectAsState()
  if (isConnectionDown(isOnline, maybeConnection)) return
  val text = stringResource(
    when (progress.stage) {
      FirstSyncStageItem.NEWEST -> R.string.first_sync_newest
      FirstSyncStageItem.OLDER -> R.string.first_sync_older
    },
  )
  Column(Modifier.fillMaxWidth().semantics(mergeDescendants = true) {}) {
    Row(
      verticalAlignment = Alignment.CenterVertically,
      horizontalArrangement = Arrangement.SpaceBetween,
      modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 6.dp),
    ) {
      Text(text, style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
      if (progress.total > 0u) {
        Text(
          pluralStringResource(
            R.plurals.first_sync_count,
            progress.total.toInt(),
            progress.done.toInt(),
            progress.total.toInt(),
          ),
          style = MaterialTheme.typography.labelMedium,
          color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
      }
    }
    val maybeFraction = progress.maybeFraction
    if (maybeFraction != null) {
      val shown by animateFloatAsState(maybeFraction, label = "first sync progress")
      LinearProgressIndicator(progress = { shown }, modifier = Modifier.fillMaxWidth())
    } else {
      LinearProgressIndicator(modifier = Modifier.fillMaxWidth())
    }
  }
}

/** How often the countdown to the next attempt (and the grace period) is checked. */
private const val COUNTDOWN_MILLIS = 1_000L

/** How long syncing goes on before [SyncingLine] shows it. */
private const val SYNCING_LINE_DELAY_MILLIS = 300L
private val SYNCING_LINE_HEIGHT = 2.dp
