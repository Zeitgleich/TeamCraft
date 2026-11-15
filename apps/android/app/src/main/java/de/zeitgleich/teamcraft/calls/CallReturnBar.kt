package de.zeitgleich.teamcraft.calls

import android.os.SystemClock
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.expandVertically
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.shrinkVertically
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBars
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Call
import androidx.compose.material3.FilledIconButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButtonDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.delay
import de.zeitgleich.teamcraft.R
import uniffi.teamcraft_ffi.CallPhase
import uniffi.teamcraft_ffi.CallStateInfo

/** How often the bar's clock ticks while it is on screen. */
private const val TICK_MS = 1000L

/** The in-call green of the system's own "back to the call" chip, dark enough for white text. */
private val BAR_COLOR = Color(0xFF1E7A50)

/**
 * The bar at the top of the app while a call runs and its screen is put away (#2066): the call's
 * name, its time (or that it reconnects), mute and hang up; a tap on the rest brings the call
 * screen back. The call never stops for this and nothing is joined again. It takes the status bar's
 * inset itself; the screens below get the rest.
 */
@Composable
fun CallReturnBar(calls: CallController, modifier: Modifier = Modifier) {
  val maybeState by calls.callState.collectAsStateWithLifecycle()
  val state = maybeState
  AnimatedVisibility(
    visible = state != null && state.phase != CallPhase.ENDED,
    enter = fadeIn() + expandVertically(expandFrom = Alignment.Top),
    exit = fadeOut() + shrinkVertically(shrinkTowards = Alignment.Top),
    modifier = modifier,
  ) {
    // The last running state stays while the bar leaves, so its text doesn't jump to "ended".
    var shown by remember { mutableStateOf(state) }
    if (state != null && state.phase != CallPhase.ENDED) shown = state
    shown?.let { BarContent(calls, it) }
  }
}

@Composable
private fun BarContent(calls: CallController, state: CallStateInfo) {
  val haptics = LocalHapticFeedback.current
  val name by produceState("", state.conversationId) { value = calls.displayName(state.conversationId) }
  val returnLabel = stringResource(R.string.call_return_to, name)
  Surface(color = BAR_COLOR, contentColor = Color.White, modifier = Modifier.fillMaxWidth()) {
    Row(
      Modifier.windowInsetsPadding(WindowInsets.statusBars).padding(start = 16.dp, end = 8.dp),
      horizontalArrangement = Arrangement.spacedBy(4.dp),
      verticalAlignment = Alignment.CenterVertically,
    ) {
      Row(
        Modifier
          .weight(1f)
          .heightIn(min = 48.dp)
          .clickable(onClickLabel = returnLabel, role = Role.Button) { calls.bringToFront() },
        horizontalArrangement = Arrangement.spacedBy(12.dp),
        verticalAlignment = Alignment.CenterVertically,
      ) {
        Icon(Icons.Filled.Call, contentDescription = null, modifier = Modifier.size(18.dp))
        Column {
          Text(name, style = MaterialTheme.typography.titleSmall, maxLines = 1, overflow = TextOverflow.Ellipsis)
          Text(statusText(calls, state), style = MaterialTheme.typography.bodySmall, maxLines = 1)
        }
      }
      FilledIconButton(
        onClick = {
          haptics.performHapticFeedback(if (state.isMuted) HapticFeedbackType.ToggleOn else HapticFeedbackType.ToggleOff)
          calls.setMuted(!state.isMuted)
        },
        colors = IconButtonDefaults.filledIconButtonColors(
          containerColor = if (state.isMuted) Color.White else Color.White.copy(alpha = 0.2f),
          contentColor = if (state.isMuted) BAR_COLOR else Color.White,
        ),
      ) {
        Icon(
          if (state.isMuted) CallIcons.MicOff else CallIcons.Mic,
          contentDescription = stringResource(if (state.isMuted) R.string.call_unmute else R.string.call_mute),
        )
      }
      FilledIconButton(
        onClick = {
          haptics.performHapticFeedback(HapticFeedbackType.Confirm)
          // Outside the call screen, as from the notification: an organizer with people still in
          // the lobby gets the reminder by notification (#1859).
          calls.hangUpFromSystem()
        },
        colors = IconButtonDefaults.filledIconButtonColors(
          containerColor = MaterialTheme.colorScheme.error,
          contentColor = MaterialTheme.colorScheme.onError,
        ),
      ) {
        Icon(CallIcons.CallEnd, contentDescription = stringResource(R.string.call_hang_up))
      }
    }
  }
}

/** The call's time once connected, "Connecting…" before, "Reconnecting…" while it rejoins. */
@Composable
private fun statusText(calls: CallController, state: CallStateInfo): String {
  val maybeConnectedAt by calls.connectedAtMs.collectAsStateWithLifecycle()
  var now by remember { mutableStateOf(SystemClock.elapsedRealtime()) }
  // Ticks only while the bar is composed, i.e. on screen.
  LaunchedEffect(maybeConnectedAt) {
    while (maybeConnectedAt != null) {
      now = SystemClock.elapsedRealtime()
      delay(TICK_MS)
    }
  }
  val connectedAt = maybeConnectedAt
  return when {
    state.phase == CallPhase.RECONNECTING -> stringResource(R.string.call_bar_reconnecting)
    state.phase == CallPhase.CONNECTED && connectedAt != null ->
      formatDuration(((now - connectedAt).coerceAtLeast(0)) / TICK_MS)
    else -> stringResource(R.string.call_connecting)
  }
}
