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
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.material3.TextButton
import androidx.compose.foundation.layout.Row
import androidx.compose.ui.Alignment
import androidx.compose.ui.platform.LocalContext
import kotlinx.coroutines.delay
import uniffi.teamcraft_ffi.ConnectionState
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
 * state; new messages wait, sent ones stay in the outbox.
 */
@Composable
fun OfflineBanner(onSignInAgain: () -> Unit) {
  val graph = TeamCraftApplication.graph(LocalContext.current)
  val isOnline = rememberIsOnline()
  val maybeConnection by graph.core.connectionState.collectAsState()
  val now by produceState(System.currentTimeMillis()) {
    while (true) {
      delay(COUNTDOWN_MILLIS)
      value = System.currentTimeMillis()
    }
  }
  var maybeConnectingSince by remember { mutableStateOf<Long?>(null) }
  LaunchedEffect(maybeConnection) {
    maybeConnectingSince = if (maybeConnection is ConnectionState.Connecting) System.currentTimeMillis() else null
  }
  val connection = maybeConnection
  val (text, maybeAction) = when {
    !isOnline -> stringResource(R.string.offline_banner) to null
    connection is ConnectionState.Offline -> {
      val seconds = ((connection.retryAtMs - now).coerceAtLeast(0) + 999) / 1000
      val text = when (connection.maybeInterception) {
        NetworkInterception.CAPTIVE_PORTAL -> R.string.connection_captive_portal
        NetworkInterception.UNTRUSTED_CERTIFICATE -> R.string.connection_untrusted_certificate
        null -> R.string.connection_unreachable
      }
      stringResource(text, seconds) to
        (stringResource(R.string.connection_retry) to { graph.core.networkChanged() })
    }
    connection is ConnectionState.SignInRequired ->
      stringResource(R.string.connection_sign_in) to (stringResource(R.string.connection_sign_in_action) to onSignInAgain)
    connection is ConnectionState.Connecting &&
      maybeConnectingSince?.let { now - it >= CONNECTING_GRACE_MILLIS } == true ->
      stringResource(R.string.connection_connecting) to null
    else -> return
  }
  Surface(
    color = MaterialTheme.colorScheme.errorContainer,
    modifier = Modifier.fillMaxWidth(),
  ) {
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
      maybeAction?.let { (label, action) ->
        TextButton(onClick = action) { Text(label) }
      }
    }
  }
}

/** "Connecting" only shows when it takes longer than this; a quick reconnect needs no banner. */
private const val CONNECTING_GRACE_MILLIS = 3_000L
private const val COUNTDOWN_MILLIS = 1_000L
