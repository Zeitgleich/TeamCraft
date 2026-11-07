package de.zeitgleich.teamcraft.ui.screens

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
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.ChatSnapshot
import de.zeitgleich.teamcraft.core.Message
import de.zeitgleich.teamcraft.core.MessageSource
import uniffi.teamcraft_ffi.NotifiedConversation

/**
 * A quiet loading line below the feed while the conversation's messages are on their way after a
 * notification opened it (issue #1065). The message the notification showed is in the feed itself
 * ([withNotified]), drawn like every other message.
 */
@Composable
fun NotifiedMessageRow(maybeNotified: NotifiedConversation?, isLoading: Boolean) {
  if (maybeNotified == null || !isLoading) return
  Row(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp), verticalAlignment = Alignment.CenterVertically) {
    CircularProgressIndicator(Modifier.size(14.dp), strokeWidth = 2.dp)
    Text(
      stringResource(R.string.messages_loading),
      style = MaterialTheme.typography.bodySmall,
      color = MaterialTheme.colorScheme.onSurfaceVariant,
      modifier = Modifier.padding(start = 8.dp),
    )
  }
}

/**
 * The feed with the message the notification showed, while the store doesn't have it yet: a message
 * like the others, with its sender, bubble and time, so it isn't a bare line that turns into a bubble
 * once the sync brings it (#1371). It has the stored message's ID, which takes its place without a
 * jump. A reply in a thread isn't part of the conversation's own feed.
 */
fun withNotified(messages: List<Message>, maybeNotified: NotifiedConversation?): List<Message> {
  val notified = maybeNotified?.maybeMessage?.takeIf { it.maybeThreadId == null } ?: return messages
  val body = notified.maybeText ?: notified.maybeFileName ?: return messages
  if (messages.any { it.id == notified.messageId }) return messages
  val message = Message(
    id = notified.messageId,
    conversation = maybeNotified.conversationId,
    author = notified.maybeSenderId.orEmpty(),
    sentAt = notified.sentAtMs,
    body = body,
    maybeSource = notified.maybeSenderName?.takeIf { notified.isSenderSource }?.let { MessageSource(name = it) },
  )
  return (messages + message).sortedBy { it.sentAt }
}

/**
 * What the feed draws: nothing until the snapshot is there (issue #1425). Without it every sender is
 * a "?" and system lines have no actor, and the bubbles change height once the names arrive.
 */
fun drawnMessages(messages: List<Message>, maybeNotified: NotifiedConversation?, maybeSnapshot: ChatSnapshot?): List<Message> =
  if (maybeSnapshot == null) emptyList() else withNotified(messages, maybeNotified)
