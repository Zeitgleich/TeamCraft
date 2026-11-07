package de.zeitgleich.teamcraft.ui

import androidx.compose.foundation.Image
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.produceState
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import uniffi.teamcraft_ffi.ConversationItem
import uniffi.teamcraft_ffi.ConversationKind
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import de.zeitgleich.teamcraft.TeamCraftApplication

/**
 * A person's round profile picture (issue #846), with [placeholder] (their initials) while it loads
 * and for people without one. The picture comes from the app's shared avatar cache; it is fetched when
 * missing or stale, and again when the device comes back online after a failed fetch. [modifier]
 * sizes and shapes the avatar, as it would the placeholder alone.
 */
@Composable
fun UserAvatar(maybeUserId: String?, modifier: Modifier = Modifier, placeholder: @Composable () -> Unit) {
  val graph = TeamCraftApplication.graph(LocalContext.current)
  val avatars = graph.avatars
  val userId = maybeUserId?.takeIf { it.isNotEmpty() }
  val changes by avatars.changes.collectAsState()
  val isOnline by graph.network.isOnline.collectAsState()
  val maybePicture by produceState<ImageBitmap?>(userId?.let(avatars::maybeDecoded)?.asImageBitmap(), userId, changes) {
    value = userId?.let { avatars.loadBitmap(it) }?.asImageBitmap()
  }
  LaunchedEffect(userId, isOnline) {
    if (userId != null && isOnline) avatars.refresh(userId)
  }
  val picture = maybePicture
  if (picture == null) {
    placeholder()
  } else {
    Box(modifier, contentAlignment = Alignment.Center) {
      Image(picture, contentDescription = null, contentScale = ContentScale.Crop, modifier = Modifier.matchParentSize().clip(CircleShape))
    }
  }
}

/**
 * A person's colour behind their initial: the hue the core gives every user (their ID modulo 360,
 * `avatar_hue`), so list, header and messages show the same person in the same colour. Without a
 * user (a group, an integration) the name decides.
 */
fun avatarColor(maybeUserId: String?, name: String): Color {
  val hue = maybeUserId?.toLongOrNull()?.mod(360L)?.toInt() ?: name.hashCode().mod(360)
  return avatarColorOfHue(hue)
}

/**
 * The colour of a conversation's avatar: the hue the core gave the other person of a one-to-one DM
 * (`maybePeer`, the desktop's colour), else [avatarColor].
 */
fun conversationAvatarColor(conversation: ConversationItem): Color =
  conversation.maybePeer?.let { avatarColorOfHue(it.avatarHue.toInt()) }
    ?: avatarColor(conversation.maybePeerId, conversation.name)

/** The conversation is a one-to-one DM with a bot or integration account (the BOT marker). */
fun ConversationItem.isWithBot(): Boolean = kind == ConversationKind.DIRECT && maybePeer?.isBot == true

/** The same from a hue the core named ([avatarColor]). */
fun avatarColorOfHue(hue: Int): Color = Color.hsl(hue.mod(360).toFloat(), AVATAR_SATURATION, AVATAR_LIGHTNESS)

private const val AVATAR_SATURATION = 0.45f
private const val AVATAR_LIGHTNESS = 0.45f
