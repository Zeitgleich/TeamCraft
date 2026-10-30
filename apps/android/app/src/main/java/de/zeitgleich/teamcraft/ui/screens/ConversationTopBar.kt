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
import de.zeitgleich.teamcraft.ui.UserAvatar
import de.zeitgleich.teamcraft.ui.theme.LocalTeamCraftColors
import de.zeitgleich.teamcraft.ui.theme.darkStatusBarBackdrop

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
  onBack: () -> Unit,
  actions: @Composable RowScope.() -> Unit,
) {
  val colors = LocalTeamCraftColors.current
  val maybePeer = maybeConversation?.takeIf { it.kind == "Direct" }?.let { conversation ->
    conversation.memberIds.firstOrNull { it != snapshot?.me?.id }?.let { snapshot?.user(it) }
  }
  val name = maybeConversation?.name.orEmpty()
  val subtitle = when {
    maybePeer != null -> peerStatus(maybePeer)
    maybeConversation == null -> ""
    maybeConversation.topic.isNotBlank() -> maybeConversation.topic
    maybeConversation.memberIds.isNotEmpty() ->
      pluralStringResource(R.plurals.conversation_members, maybeConversation.memberIds.size, maybeConversation.memberIds.size)
    else -> ""
  }
  TopAppBar(
    title = {
      Row(verticalAlignment = Alignment.CenterVertically) {
        HeaderAvatar(name, maybeConversation?.kind, maybePeer)
        Column(Modifier.padding(start = 12.dp)) {
          Text(
            name,
            style = MaterialTheme.typography.titleLarge,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.semantics { heading() },
          )
          if (subtitle.isNotBlank()) {
            Text(
              subtitle,
              maxLines = 1,
              overflow = TextOverflow.Ellipsis,
              style = MaterialTheme.typography.bodySmall,
              color = colors.chromeMuted,
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

/** The person's picture or initial on their hue with a presence dot, or the channel's marker. */
@Composable
private fun HeaderAvatar(name: String, maybeKind: String?, maybePeer: User?) {
  val colors = LocalTeamCraftColors.current
  val isPeople = maybeKind == "Direct" || maybeKind == "Group"
  val label = when (maybeKind) {
    "Public" -> "#"
    "Private" -> "◆"
    else -> name.firstOrNull()?.uppercase() ?: "?"
  }
  val hue = (maybePeer?.avatarHue ?: name.hashCode()).mod(360).toFloat()
  Box {
    UserAvatar(maybePeer?.id, Modifier.size(HEADER_AVATAR)) {
      Box(
        Modifier
          .size(HEADER_AVATAR)
          .background(if (isPeople) Color.hsl(hue, 0.45f, 0.45f) else colors.chromeText.copy(alpha = 0.14f), CircleShape),
        contentAlignment = Alignment.Center,
      ) {
        Text(
          label,
          style = MaterialTheme.typography.titleSmall,
          color = colors.chromeText,
          modifier = Modifier.clearAndSetSemantics {},
        )
      }
    }
    maybePeer?.let { peer ->
      Box(
        Modifier
          .align(Alignment.BottomEnd)
          .size(12.dp)
          .background(colors.chrome, CircleShape)
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

private val HEADER_AVATAR = 36.dp
