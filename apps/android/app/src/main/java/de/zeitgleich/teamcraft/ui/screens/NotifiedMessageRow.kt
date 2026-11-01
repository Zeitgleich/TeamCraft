package de.zeitgleich.teamcraft.ui.screens

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import uniffi.teamcraft_ffi.NotifiedConversation

/**
 * The message the notification showed, below the feed while the store doesn't have it yet, and a
 * quiet loading line while the conversation's messages are on their way (issue #1065). Opening a
 * conversation from a notification shows what the device already knows, never an empty screen.
 */
@Composable
fun NotifiedMessageRow(maybeNotified: NotifiedConversation?, isLoading: Boolean) {
  val notified = maybeNotified ?: return
  // A reply in a thread isn't part of the conversation's own feed.
  val maybeMessage = notified.maybeMessage?.takeIf { it.maybeThreadId == null }
  val maybeBody = maybeMessage?.let { it.maybeText ?: it.maybeFileName }
  Column(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp)) {
    if (maybeMessage != null && maybeBody != null) {
      maybeMessage.maybeSenderName?.let { sender ->
        Text(sender, style = MaterialTheme.typography.labelLarge, fontWeight = FontWeight.SemiBold)
      }
      Text(maybeBody, style = MaterialTheme.typography.bodyLarge)
    }
    if (isLoading) {
      Row(Modifier.padding(top = 8.dp), verticalAlignment = Alignment.CenterVertically) {
        CircularProgressIndicator(Modifier.size(14.dp), strokeWidth = 2.dp)
        Text(
          stringResource(R.string.messages_loading),
          style = MaterialTheme.typography.bodySmall,
          color = MaterialTheme.colorScheme.onSurfaceVariant,
          modifier = Modifier.padding(start = 8.dp),
        )
      }
    }
  }
}
