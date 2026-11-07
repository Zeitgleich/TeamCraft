package de.zeitgleich.teamcraft.ui.screens

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.filled.Lock
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.ChatSnapshot
import de.zeitgleich.teamcraft.core.Conversation
import de.zeitgleich.teamcraft.core.User
import de.zeitgleich.teamcraft.ui.avatarColorOfHue
import de.zeitgleich.teamcraft.ui.avatarColor
import de.zeitgleich.teamcraft.ui.TypingDots
import de.zeitgleich.teamcraft.ui.UserAvatar
import uniffi.teamcraft_ffi.ConversationItem
import uniffi.teamcraft_ffi.ConversationKind
import uniffi.teamcraft_ffi.NotifiedConversation

/**
 * The conversation's header (issue #704), like the desktop's: back, a round avatar (with the
 * person's presence in a direct message), the name and one quiet status line (custom status or
 * presence, the topic or the member count), then the actions.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ConversationTopBar(
  snapshot: ChatSnapshot?,
  maybeConversation: Conversation?,
  maybeNotified: NotifiedConversation?,
  /** The chat list's row: name and kind until the snapshot is read (issue #1425). */
  maybeListed: ConversationItem?,
  onBack: () -> Unit,
  /** Who types in this conversation itself right now (#882); in a direct message the status gives way. */
  typing: List<String> = emptyList(),
  actions: @Composable RowScope.() -> Unit,
) {
  val maybePeer = maybeConversation?.takeIf { it.kind == "Direct" }?.let { conversation ->
    conversation.memberIds.firstOrNull { it != snapshot?.me?.id }?.let { snapshot?.user(it) }
  }
  // Not stored yet (opened from a notification, issue #1065): the name the notification showed.
  val name = maybeConversation?.name ?: maybeListed?.name ?: maybeNotified?.maybeName.orEmpty()
  val maybeKind = maybeConversation?.kind ?: maybeListed?.let { wireKind(it.kind) } ?: maybeNotified?.let { if (it.isDirect) "Direct" else null }
  val subtitle = when {
    maybePeer != null -> peerStatus(maybePeer)
    // Not read yet: no loading line for what the store has, it shows in a moment.
    snapshot == null -> maybeListed?.topic.orEmpty()
    maybeConversation == null -> stringResource(R.string.conversation_loading)
    maybeConversation.topic.isNotBlank() -> maybeConversation.topic
    maybeConversation.memberIds.isNotEmpty() ->
      pluralStringResource(R.plurals.conversation_members, maybeConversation.memberIds.size, maybeConversation.memberIds.size)
    else -> ""
  }
  val isTypingInDirect = maybePeer != null && typing.isNotEmpty()
  TopAppBar(
    title = {
      Row(verticalAlignment = Alignment.CenterVertically) {
        HeaderAvatar(name, maybeKind, maybePeer)
        Column(Modifier.padding(start = 12.dp)) {
          Text(
            name,
            style = MaterialTheme.typography.titleMedium.copy(fontWeight = FontWeight.SemiBold),
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.semantics { heading() },
          )
          if (isTypingInDirect) {
            Row(verticalAlignment = Alignment.CenterVertically) {
              TypingDots()
              Text(
                stringResource(R.string.typing_direct),
                modifier = Modifier.padding(start = 6.dp),
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                style = MaterialTheme.typography.bodySmall,
                fontStyle = FontStyle.Italic,
                color = MaterialTheme.colorScheme.primary,
              )
            }
          } else if (subtitle.isNotBlank()) {
            Text(
              subtitle,
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
      IconButton(onClick = onBack) {
        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = stringResource(R.string.back))
      }
    },
    actions = actions,
    windowInsets = TopBarInsets,
    // The theme's surface, as on the main screen: one header style in light and dark (the status
    // bar icons follow the theme).
    colors = TopAppBarDefaults.topAppBarColors(containerColor = MaterialTheme.colorScheme.surface),
  )
}

/** A person's custom status, or else their presence. */
@Composable
private fun peerStatus(peer: User): String {
  peer.maybeCustomStatus?.let { status ->
    val text = listOf(status.emoji, status.text).filter { it.isNotBlank() }.joinToString(" ")
    if (text.isNotBlank()) return text
  }
  return stringResource(
    when (peer.presence) {
      "Online" -> R.string.presence_online
      "Away" -> R.string.presence_away
      "DoNotDisturb" -> R.string.presence_dnd
      else -> R.string.presence_peer_offline
    },
  )
}

/**
 * The person's picture or initial on their hue with a presence dot, or the channel's marker: a
 * rounded square with `#` or a lock, as in the lists.
 */
@Composable
private fun HeaderAvatar(name: String, maybeKind: String?, maybePeer: User?) {
  val scheme = MaterialTheme.colorScheme
  if (maybeKind == "Public" || maybeKind == "Private") {
    Box(
      Modifier.size(HEADER_AVATAR).background(scheme.surfaceContainerHighest, RoundedCornerShape(10.dp)),
      contentAlignment = Alignment.Center,
    ) {
      if (maybeKind == "Private") {
        Icon(Icons.Filled.Lock, contentDescription = null, tint = scheme.onSurfaceVariant, modifier = Modifier.size(18.dp))
      } else {
        Text("#", style = MaterialTheme.typography.titleSmall.copy(fontWeight = FontWeight.SemiBold), color = scheme.onSurfaceVariant)
      }
    }
    return
  }
  // Without a name yet an empty circle, never a question mark.
  val label = name.firstOrNull()?.uppercase().orEmpty()
  val personColor = maybePeer?.let { avatarColorOfHue(it.avatarHue) } ?: avatarColor(null, name)
  Box {
    UserAvatar(maybePeer?.id, Modifier.size(HEADER_AVATAR)) {
      Box(Modifier.size(HEADER_AVATAR).background(personColor, CircleShape), contentAlignment = Alignment.Center) {
        Text(label, style = MaterialTheme.typography.titleSmall, color = Color.White, modifier = Modifier.clearAndSetSemantics {})
      }
    }
    maybePeer?.let { peer ->
      Box(
        Modifier
          .align(Alignment.BottomEnd)
          .size(12.dp)
          .background(scheme.surface, CircleShape)
          .padding(2.dp)
          .background(presenceDotColor(peer.presence), CircleShape),
      )
    }
  }
}

private fun presenceDotColor(presence: String): Color = when (presence) {
  "Online" -> Color(0xFF2E9E5B)
  "Away" -> Color(0xFFE0A030)
  "DoNotDisturb" -> Color(0xFFD0453A)
  else -> Color(0xFF8A8F98)
}

/** The snapshot's name of a [ConversationKind], as [Conversation.kind] holds it. */
private fun wireKind(kind: ConversationKind): String? = when (kind) {
  ConversationKind.PUBLIC -> "Public"
  ConversationKind.PRIVATE -> "Private"
  ConversationKind.DIRECT -> "Direct"
  ConversationKind.GROUP -> "Group"
  ConversationKind.INTEGRATION -> null
}

private val HEADER_AVATAR = 36.dp
