package de.zeitgleich.teamcraft.ui.screens

import android.text.format.DateUtils
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Call
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.CallTile

/**
 * A call in the conversation (docs/notifications.md, "Calls in the timeline"): a tile at its start
 * with who started it, the time, and "Ongoing" with Join while it runs, or its duration once over.
 * A channel's call is a huddle, as desktop and web name it.
 */
@Composable
internal fun CallTileRow(
  call: CallTile,
  starterName: String,
  sentAt: Long,
  isChannel: Boolean,
  onJoin: () -> Unit,
) {
  val context = LocalContext.current
  val isOngoing = call.state.isOngoing
  val title = stringResource(
    when {
      call.isMissed -> R.string.call_tile_missed
      isChannel -> R.string.call_tile_huddle
      else -> R.string.call_tile_call
    },
  )
  val stateText = when {
    isOngoing -> stringResource(R.string.call_tile_ongoing)
    else -> call.state.maybeDurationMs?.let(::callDuration) ?: stringResource(R.string.call_tile_ended)
  }
  val detail = if (starterName.isEmpty()) stateText else "$starterName · $stateText"
  val time = DateUtils.formatDateTime(context, sentAt, DateUtils.FORMAT_SHOW_TIME)
  val colors = MaterialTheme.colorScheme
  val accent = when {
    call.isMissed -> colors.error
    isOngoing -> colors.primary
    else -> colors.onSurfaceVariant
  }
  Row(
    Modifier
      .fillMaxWidth()
      .padding(horizontal = 16.dp, vertical = 4.dp)
      .widthIn(max = 420.dp)
      .border(1.dp, colors.outlineVariant, RoundedCornerShape(12.dp))
      .padding(horizontal = 12.dp, vertical = 10.dp)
      .semantics(mergeDescendants = true) { contentDescription = "$title, $detail, $time" },
    verticalAlignment = Alignment.CenterVertically,
    horizontalArrangement = Arrangement.spacedBy(12.dp),
  ) {
    Box(
      Modifier.size(32.dp).background(if (isOngoing) colors.primaryContainer else colors.surfaceVariant, CircleShape),
      contentAlignment = Alignment.Center,
    ) {
      Icon(Icons.Filled.Call, contentDescription = null, tint = accent, modifier = Modifier.size(18.dp))
    }
    Column(Modifier.weight(1f)) {
      Text(
        title,
        style = MaterialTheme.typography.titleSmall,
        fontWeight = FontWeight.SemiBold,
        color = if (call.isMissed) colors.error else colors.onSurface,
      )
      Text(
        detail,
        style = MaterialTheme.typography.bodySmall,
        color = colors.onSurfaceVariant,
        maxLines = 1,
        overflow = TextOverflow.Ellipsis,
      )
    }
    if (isOngoing) {
      FilledTonalButton(onClick = onJoin) { Text(stringResource(R.string.call_tile_join)) }
    }
    Text(time, style = MaterialTheme.typography.labelSmall, color = colors.onSurfaceVariant)
  }
}

/** "3:05" or "1:02:03" for a call of that length, like desktop and web. */
internal fun callDuration(durationMs: Long): String {
  val seconds = durationMs.coerceAtLeast(0) / 1000
  val hours = seconds / 3600
  val minutes = seconds % 3600 / 60
  val rest = seconds % 60
  return if (hours > 0) "%d:%02d:%02d".format(hours, minutes, rest) else "%d:%02d".format(minutes, rest)
}
