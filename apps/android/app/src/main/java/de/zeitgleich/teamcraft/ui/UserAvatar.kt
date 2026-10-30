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
