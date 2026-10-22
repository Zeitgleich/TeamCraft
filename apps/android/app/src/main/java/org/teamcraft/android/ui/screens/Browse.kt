package org.teamcraft.android.ui.screens

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Tab
import androidx.compose.material3.TabRow
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import org.teamcraft.android.R
import org.teamcraft.android.core.BrowsableChannel
import org.teamcraft.android.core.ChatActions
import org.teamcraft.android.core.ChatQueries
import org.teamcraft.android.core.ChatSnapshot
import org.teamcraft.android.ui.MainViewModel

/** Public channels of each space to join, a new channel, and people to message directly. */
@Composable
fun BrowseScreen(viewModel: MainViewModel, onOpen: (String) -> Unit, onBack: () -> Unit) {
  val changes by viewModel.changeCount.collectAsState()
  var snapshot by remember { mutableStateOf<ChatSnapshot?>(null) }
  var spaceIndex by rememberSaveable { mutableIntStateOf(0) }
  var showPeople by rememberSaveable { mutableStateOf(false) }
  var channels by remember { mutableStateOf<List<BrowsableChannel>>(emptyList()) }
  var isCreating by remember { mutableStateOf(false) }
  val spaces = snapshot?.spaces.orEmpty()
  val maybeSpace = spaces.getOrNull(spaceIndex)
  LaunchedEffect(changes, maybeSpace?.id) {
    snapshot = viewModel.readChat { snapshot() }
    channels = maybeSpace?.let { space -> viewModel.readChat { query(ChatQueries.browseChannels(space.id)).channels } } ?: emptyList()
  }
  Scaffold(topBar = { ChromeTopBar(title = stringResource(R.string.browse_title), maybeOnBack = onBack) }) { padding ->
    Column(Modifier.fillMaxSize().padding(padding)) {
      TabRow(selectedTabIndex = if (showPeople) 1 else 0) {
        Tab(selected = !showPeople, onClick = { showPeople = false }, text = { Text(stringResource(R.string.channels)) })
        Tab(selected = showPeople, onClick = { showPeople = true }, text = { Text(stringResource(R.string.people)) })
      }
      if (showPeople) {
        LazyColumn(Modifier.fillMaxSize()) {
          items(snapshot?.users.orEmpty().filter { it.id != snapshot?.me?.id && !it.isBot }, key = { it.id }) { user ->
            Row(
              Modifier.fillMaxWidth().clickable { viewModel.act(ChatActions.openDm(user.id)) }.padding(16.dp),
              verticalAlignment = Alignment.CenterVertically,
            ) {
              Column(Modifier.weight(1f)) {
                Text(user.displayName)
                Text(
                  listOfNotNull("@" + user.username, user.maybeCustomStatus?.let { it.emoji + " " + it.text }).joinToString(" · "),
                  style = MaterialTheme.typography.bodySmall,
                  color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
              }
              Text(presenceDot(user.presence))
            }
            HorizontalDivider()
          }
        }
      } else {
        if (spaces.size > 1) {
          TabRow(selectedTabIndex = spaceIndex.coerceAtMost(spaces.lastIndex)) {
            spaces.forEachIndexed { index, space ->
              Tab(selected = index == spaceIndex, onClick = { spaceIndex = index }, text = { Text(space.name) })
            }
          }
        }
        if (maybeSpace != null) {
          TextButton(onClick = { isCreating = true }, modifier = Modifier.padding(horizontal = 8.dp)) {
            Text(stringResource(R.string.channel_create))
          }
        }
        LazyColumn(Modifier.fillMaxSize()) {
          items(channels.filter { !it.isArchived }, key = { it.conversation }) { channel ->
            Row(
              Modifier.fillMaxWidth().clickable(enabled = channel.isMember) { onOpen(channel.conversation) }.padding(16.dp),
              verticalAlignment = Alignment.CenterVertically,
            ) {
              Column(Modifier.weight(1f)) {
                Text("# " + channel.name + if (channel.hasExternalPeople) " · " + stringResource(R.string.external_people) else "")
                if (channel.topic.isNotBlank()) {
                  Text(channel.topic, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
              }
              if (channel.isMember) {
                Text(stringResource(R.string.channel_member), style = MaterialTheme.typography.labelSmall)
              } else {
                OutlinedButton(onClick = { viewModel.act(ChatActions.joinChannel(channel.conversation)) }) {
                  Text(stringResource(R.string.channel_join))
                }
              }
            }
            HorizontalDivider()
          }
        }
      }
    }
  }
  if (isCreating && maybeSpace != null) {
    TextDialog(
      title = R.string.channel_create,
      initial = "",
      onConfirm = { name ->
        viewModel.act(ChatActions.createChannel(maybeSpace.id, name.trim()))
        isCreating = false
      },
      onDismiss = { isCreating = false },
    )
  }
}

/** A small marker for a presence state as the core reports it. */
fun presenceDot(presence: String): String = when (presence) {
  "Online" -> "🟢"
  "Away" -> "🟡"
  "DoNotDisturb" -> "⛔"
  else -> "⚪"
}
