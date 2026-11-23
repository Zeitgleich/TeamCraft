package de.zeitgleich.teamcraft.ui.screens

import de.zeitgleich.teamcraft.ui.theme.LocalMotion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.heading
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3ExpressiveApi
import androidx.compose.material3.LoadingIndicator
import androidx.compose.material3.LoadingIndicatorDefaults
import androidx.compose.ui.graphics.Color
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
import androidx.compose.animation.animateColorAsState
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.statusBars
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.liveRegion
import de.zeitgleich.teamcraft.ui.ConnectionStrip
import de.zeitgleich.teamcraft.ui.connectionStrip
import de.zeitgleich.teamcraft.ui.needsBanner
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.calculateEndPadding
import androidx.compose.foundation.layout.calculateStartPadding
import androidx.compose.ui.platform.LocalLayoutDirection
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

/**
 * Edge to edge for a screen with a fixed header above its list: the header side of a Scaffold's
 * padding (top and sides) pads the column, and [bottomOnly] goes to the list as content padding,
 * so the list scrolls behind the navigation bar instead of being clipped above it.
 */
@Composable
fun PaddingValues.withoutBottom(): PaddingValues {
  val direction = LocalLayoutDirection.current
  return PaddingValues(start = calculateStartPadding(direction), top = calculateTopPadding(), end = calculateEndPadding(direction))
}

/** The bottom of a Scaffold's padding (navigation bar or keyboard): a list's last item ends clear of it. */
fun PaddingValues.bottomOnly(): PaddingValues = PaddingValues(bottom = calculateBottomPadding())

/**
 * The top bar of the screens behind the main one: back, title and actions on the theme's surface.
 * A subtitle uses Material's expressive two-line bar (issue #454).
 */
@Composable
fun ChromeTopBar(
  title: String,
  maybeSubtitle: String? = null,
  maybeOnBack: (() -> Unit)? = null,
  actions: @Composable RowScope.() -> Unit = {},
  containerColor: Color = MaterialTheme.colorScheme.surface,
  maybeIcon: (@Composable () -> Unit)? = null,
) {
  val titleSlot = @Composable {
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
      maybeIcon?.invoke()
      Text(title, maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.semantics { heading() })
    }
  }
  val navigationIcon = @Composable {
    if (maybeOnBack != null) {
      IconButton(onClick = maybeOnBack) {
        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = stringResource(R.string.back))
      }
    }
  }
  // The theme's surface by default, as on the main screen: one header style in light and dark (the
  // status bar icons follow the theme); pages on another colour (settings) pass theirs.
  val colors = TopAppBarDefaults.topAppBarColors(containerColor = containerColor)
  val maybeSubtitleText = maybeSubtitle?.takeIf { it.isNotBlank() }
  if (maybeSubtitleText == null) {
    TopAppBar(
      title = titleSlot,
      navigationIcon = navigationIcon,
      actions = actions,
      windowInsets = TopBarInsets,
      colors = colors,
    )
  } else {
    TopAppBar(
      title = titleSlot,
      subtitle = { Text(maybeSubtitleText, maxLines = 1, overflow = TextOverflow.Ellipsis) },
      navigationIcon = navigationIcon,
      actions = actions,
      windowInsets = TopBarInsets,
      colors = colors,
    )
  }
}

/** The connection notice for this moment, following the core, the network and the grace period. */
@Composable
private fun rememberConnectionNotice(): ConnectionNotice? {
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
  return connectionNotice(isOnline, maybeConnection, unsettled, now, isSyncStartFailing)
}

/**
 * The connection state along the top edge of the screen, just below the status bar: a thin red bar
 * while the app can't reach the server (no network, the server down or restarting), yellow while it
 * connects, nothing once connected. A connection that comes back within the grace period shows
 * nothing ([CONNECTION_GRACE_MS]; longer after the app's return). Messages wait in the outbox
 * meanwhile. Screen readers hear the state once as it changes.
 */
@Composable
fun ConnectionStripBar(modifier: Modifier = Modifier) {
  val maybeNotice = rememberConnectionNotice()
  val maybeStrip = connectionStrip(maybeNotice)
  val color by animateColorAsState(
    when (maybeStrip) {
      ConnectionStrip.DOWN -> MaterialTheme.colorScheme.error
      ConnectionStrip.CONNECTING -> CONNECTING_YELLOW
      null -> Color.Transparent
    },
    LocalMotion.current.fast(),
    label = "connection strip",
  )
  val description = when (maybeNotice) {
    ConnectionNotice.DeviceOffline -> stringResource(R.string.offline_banner)
    ConnectionNotice.SyncNotStarted -> stringResource(R.string.connection_sync_not_started)
    ConnectionNotice.Connecting -> stringResource(R.string.connection_connecting)
    is ConnectionNotice.Unreachable -> stringResource(R.string.connection_server_unreachable)
    else -> null
  }.takeIf { maybeStrip != null }
  Box(
    modifier
      .fillMaxWidth()
      .windowInsetsPadding(WindowInsets.statusBars)
      .height(CONNECTION_STRIP_HEIGHT)
      .background(color)
      .semantics {
        description?.let {
          contentDescription = it
          liveRegion = LiveRegionMode.Polite
        }
      },
  )
}

/**
 * A banner with text and an action, only while the user must do something: sign in again, sign in
 * to the network (a captive portal), or sort out an untrusted certificate. Everything the app
 * handles by itself is the strip along the top ([ConnectionStripBar]).
 */
@Composable
fun OfflineBanner(onSignInAgain: () -> Unit) {
  val graph = TeamCraftApplication.graph(LocalContext.current)
  val notice = rememberConnectionNotice()?.takeIf(::needsBanner) ?: return
  val (text, action) = when (notice) {
    is ConnectionNotice.Unreachable -> {
      val text = when (notice.maybeInterception) {
        NetworkInterception.CAPTIVE_PORTAL -> R.string.connection_captive_portal
        NetworkInterception.UNTRUSTED_CERTIFICATE -> R.string.connection_untrusted_certificate
        null -> return
      }
      stringResource(text, notice.retryInSeconds.toInt()) to
        (stringResource(R.string.connection_retry) to { graph.core.networkChanged() })
    }
    ConnectionNotice.SignInRequired ->
      stringResource(R.string.connection_sign_in) to (stringResource(R.string.connection_sign_in_action) to onSignInAgain)
    ConnectionNotice.NoRoleLeft ->
      stringResource(R.string.connection_no_role_left) to (stringResource(R.string.connection_sign_in_action) to onSignInAgain)
    // The strip's (needsBanner).
    ConnectionNotice.DeviceOffline, ConnectionNotice.Connecting, ConnectionNotice.SyncNotStarted, ConnectionNotice.Slow -> return
  }
  Surface(color = MaterialTheme.colorScheme.errorContainer, modifier = Modifier.fillMaxWidth()) {
    Row(
      verticalAlignment = Alignment.CenterVertically,
      modifier = Modifier.padding(start = 16.dp, end = 4.dp),
    ) {
      Text(
        text,
        style = MaterialTheme.typography.labelMedium,
        color = MaterialTheme.colorScheme.onErrorContainer,
        modifier = Modifier.weight(1f).padding(vertical = 6.dp),
      )
      val (label, onClick) = action
      TextButton(onClick = onClick) { Text(label) }
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
  // Connecting is the strip's at the top (ConnectionStripBar): this line is only the catch-up.
  val isSyncing = isOnline && isCatchingUp && maybeConnection !is ConnectionState.Connecting
  val isShown by produceState(false, isSyncing) {
    if (isSyncing) delay(SYNCING_LINE_DELAY_MILLIS)
    value = isSyncing
  }
  val alpha by animateFloatAsState(if (isShown) 1f else 0f, if (isShown) LocalMotion.current.fast() else LocalMotion.current.exit(), label = "syncing line")
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
      val shown by animateFloatAsState(maybeFraction, LocalMotion.current.base(), label = "first sync progress")
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

/** The connection strip along the top: thin, but clearly coloured. */
private val CONNECTION_STRIP_HEIGHT = 4.dp

/** The strip while the app connects: a warm yellow that reads on light and dark backgrounds. */
private val CONNECTING_YELLOW = Color(0xFFF2B705)

/**
 * Material 3 Expressive's loading indicator (issue #454) for a screen or pane whose content is
 * coming: the morphing shape instead of a spinner. Small waits inside buttons and rows keep the
 * thin circular indicator.
 */
@OptIn(ExperimentalMaterial3ExpressiveApi::class)
@Composable
fun PageLoadingIndicator(
  modifier: Modifier = Modifier,
  color: Color = LoadingIndicatorDefaults.indicatorColor,
) {
  LoadingIndicator(modifier = modifier, color = color)
}
