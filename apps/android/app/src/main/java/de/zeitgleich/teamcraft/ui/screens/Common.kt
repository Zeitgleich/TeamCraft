package de.zeitgleich.teamcraft.ui.screens

import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.heading
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
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
import uniffi.teamcraft_ffi.NetworkInterception
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.WindowInsetsSides
import androidx.compose.foundation.layout.only
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.ui.Modifier
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
import de.zeitgleich.teamcraft.ui.rememberIsOnline
import de.zeitgleich.teamcraft.ui.theme.LocalTeamCraftColors
import de.zeitgleich.teamcraft.ui.theme.darkStatusBarBackdrop

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

/** The dark top bar of the desktop's chrome. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ChromeTopBar(
  title: String,
  maybeSubtitle: String? = null,
  maybeOnBack: (() -> Unit)? = null,
  actions: @Composable RowScope.() -> Unit = {},
) {
  val colors = LocalTeamCraftColors.current
  TopAppBar(
    title = {
      androidx.compose.foundation.layout.Column {
        Text(title, maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.semantics { heading() })
        maybeSubtitle?.takeIf { it.isNotBlank() }?.let {
          Text(
            it,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            style = MaterialTheme.typography.bodySmall,
            color = colors.chromeMuted,
          )
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
    // Light status-bar icons over the dark chrome, also in light mode.
    modifier = Modifier.darkStatusBarBackdrop(),
    colors = TopAppBarDefaults.topAppBarColors(
      containerColor = colors.chrome,
      titleContentColor = colors.chromeText,
      navigationIconContentColor = colors.chromeText,
      actionIconContentColor = colors.chromeText,
    ),
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
  // The clock only matters while the connection is down; a connected app doesn't wake every second.
  val now by produceState(System.currentTimeMillis(), unsettled) {
    value = System.currentTimeMillis()
    while (unsettled.maybeSinceMs != null) {
      delay(COUNTDOWN_MILLIS)
      value = System.currentTimeMillis()
    }
  }
  val notice = connectionNotice(isOnline, maybeConnection, unsettled, now) ?: return
  val (text, maybeAction) = when (notice) {
    ConnectionNotice.DeviceOffline -> stringResource(R.string.offline_banner) to null
    ConnectionNotice.Connecting -> stringResource(R.string.connection_connecting) to null
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

/** How often the countdown to the next attempt (and the grace period) is checked. */
private const val COUNTDOWN_MILLIS = 1_000L
