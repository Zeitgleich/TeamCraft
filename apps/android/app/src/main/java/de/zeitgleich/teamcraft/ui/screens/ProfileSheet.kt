package de.zeitgleich.teamcraft.ui.screens

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.compositionLocalOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.ChatSnapshot
import de.zeitgleich.teamcraft.core.User
import de.zeitgleich.teamcraft.ui.MainViewModel
import de.zeitgleich.teamcraft.ui.PresenceDot
import de.zeitgleich.teamcraft.ui.UserAvatar
import de.zeitgleich.teamcraft.ui.avatarColorOfHue
import de.zeitgleich.teamcraft.ui.forms.CreateResult
import de.zeitgleich.teamcraft.ui.presenceLabel
import de.zeitgleich.teamcraft.ui.presenceOf
import de.zeitgleich.teamcraft.ui.theme.Spacing
import kotlinx.coroutines.launch

/**
 * How a tap on an @mention or a sender's avatar opens that person's profile (#2002). The feed
 * provides it; without a feed around (a thread preview) the names stay plain.
 */
class OpenProfile(val byId: (String) -> Unit, val byUsername: (String) -> Unit)

val LocalOpenProfile = compositionLocalOf<OpenProfile?> { null }

/**
 * A person's profile as a sheet: name, @username, presence, custom status, and the two things
 * one does next: message them and call them.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ProfileSheet(
  viewModel: MainViewModel,
  snapshot: ChatSnapshot?,
  user: User,
  onOpenConversation: (String) -> Unit,
  onDismiss: () -> Unit,
) {
  val scope = rememberCoroutineScope()
  var isOpening by remember { mutableStateOf(false) }
  val isMe = user.id == snapshot?.me?.id
  // The conversation with this person, found or made; then the action the button stands for.
  val withDirect: (suspend (String) -> Unit) -> Unit = { action ->
    if (!isOpening && snapshot != null) {
      isOpening = true
      scope.launch {
        openDirectConversation(viewModel, snapshot, user.id)?.let { action(it) }
        isOpening = false
      }
    }
  }
  ModalBottomSheet(onDismissRequest = onDismiss) {
    Column(
      Modifier.fillMaxWidth().padding(horizontal = Spacing.l).padding(bottom = Spacing.xxl),
      horizontalAlignment = Alignment.CenterHorizontally,
      verticalArrangement = Arrangement.spacedBy(Spacing.s),
    ) {
      UserAvatar(user.id, Modifier.size(PROFILE_AVATAR_SIZE)) {
        Box(
          Modifier.size(PROFILE_AVATAR_SIZE).background(avatarColorOfHue(user.avatarHue), CircleShape),
          contentAlignment = Alignment.Center,
        ) {
          Text(
            user.displayName.firstOrNull()?.uppercase() ?: "?",
            style = MaterialTheme.typography.headlineMedium,
            color = Color.White,
          )
        }
      }
      Text(
        user.displayName,
        style = MaterialTheme.typography.titleLarge,
        textAlign = TextAlign.Center,
        modifier = Modifier.semantics { heading() },
      )
      if (user.username.isNotEmpty()) {
        Text("@${user.username}", style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
      }
      val presence = presenceOf(user.presence)
      Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(Spacing.s)) {
        PresenceDot(presence)
        Text(stringResource(presenceLabel(presence)), style = MaterialTheme.typography.bodyMedium)
      }
      user.maybeCustomStatus?.takeIf { it.text.isNotBlank() }?.let { status ->
        // The status's emoji is the person's own content, not an icon of the app.
        Text("${status.emoji} ${status.text}".trim(), style = MaterialTheme.typography.bodyMedium, textAlign = TextAlign.Center)
      }
      if (!isMe && user.isOffered && !user.isBot) {
        Row(Modifier.fillMaxWidth().padding(top = Spacing.m), horizontalArrangement = Arrangement.spacedBy(Spacing.m)) {
          Button(
            onClick = { withDirect { id -> onDismiss(); onOpenConversation(id) } },
            enabled = !isOpening,
            modifier = Modifier.weight(1f),
          ) { Text(stringResource(R.string.profile_message)) }
          OutlinedButton(
            onClick = { withDirect { id -> onDismiss(); viewModel.startCall(id, user.displayName) } },
            enabled = !isOpening,
            modifier = Modifier.weight(1f),
          ) { Text(stringResource(R.string.profile_call)) }
        }
      }
    }
  }
}

/** The direct conversation with [userId]: the existing one, or a new one; null after telling the user why not. */
suspend fun openDirectConversation(viewModel: MainViewModel, snapshot: ChatSnapshot, userId: String): String? {
  directWith(snapshot, listOf(userId))?.let { return it }
  return when (val result = viewModel.tryCreate { openDirect(listOf(userId)) }) {
    is CreateResult.Created -> result.id
    is CreateResult.NameRefused -> {
      viewModel.showFailure(R.string.error_generic)
      null
    }
    is CreateResult.Failed -> {
      viewModel.showFailure(result.text)
      null
    }
  }
}

private val PROFILE_AVATAR_SIZE = 96.dp
