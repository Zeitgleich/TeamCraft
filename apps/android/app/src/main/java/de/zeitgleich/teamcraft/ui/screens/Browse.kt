package de.zeitgleich.teamcraft.ui.screens

import androidx.annotation.StringRes
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Lock
import androidx.compose.material3.Icon
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.selection.toggleable
import androidx.compose.material3.Button
import androidx.compose.material3.Checkbox
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.Tab
import androidx.compose.material3.PrimaryTabRow
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
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.ui.BrowseJoin
import de.zeitgleich.teamcraft.core.BrowsableChannel
import de.zeitgleich.teamcraft.core.ChatActions
import de.zeitgleich.teamcraft.core.ChatQueries
import de.zeitgleich.teamcraft.core.ChatSnapshot
import de.zeitgleich.teamcraft.ui.MainViewModel
import de.zeitgleich.teamcraft.ui.toggled

/**
 * Channels of each space to join (public ones, and private ones a workspace admin may ask to join),
 * a new channel, and people to message directly.
 */
@Composable
fun BrowseScreen(viewModel: MainViewModel, onOpen: (String) -> Unit, onBack: () -> Unit, isPeopleFirst: Boolean = false) {
  val changes by viewModel.changeCount.collectAsState()
  var snapshot by remember { mutableStateOf<ChatSnapshot?>(null) }
  var spaceIndex by rememberSaveable { mutableIntStateOf(0) }
  var showPeople by rememberSaveable { mutableStateOf(isPeopleFirst) }
  var channels by remember { mutableStateOf<List<BrowsableChannel>>(emptyList()) }
  var isCreating by remember { mutableStateOf(false) }
  val pendingJoins by viewModel.joinRequests.pending.collectAsState()
  val snackbars = rememberFailureSnackbars(viewModel)
  val spaces = snapshot?.spaces.orEmpty()
  val maybeSpace = spaces.getOrNull(spaceIndex)
  // Guests browse no channels (the server refuses them, as on desktop): they only find people.
  val isGuest = snapshot?.me?.isGuest == true
  val isShowingPeople = showPeople || isGuest
  // People picked for a new DM (one) or group DM (several).
  var picked by rememberSaveable { mutableStateOf(listOf<String>()) }
  // A new direct conversation opens as soon as the core created it, as on desktop.
  var maybePendingDm by rememberSaveable { mutableStateOf<List<String>?>(null) }
  LaunchedEffect(changes, maybeSpace?.id) {
    val loaded = viewModel.readChat { snapshot() }
    snapshot = loaded
    channels = maybeSpace
      ?.takeIf { loaded?.me?.isGuest != true }
      ?.let { space -> viewModel.readChat { query(ChatQueries.browseChannels(space.id)).channels } }
      ?: emptyList()
    val maybeCreated = maybePendingDm?.let { users -> loaded?.let { directWith(it, users) } }
    if (maybeCreated != null) {
      maybePendingDm = null
      onOpen(maybeCreated)
    }
  }
  val startConversation: () -> Unit = {
    val users = picked
    val maybeExisting = snapshot?.let { directWith(it, users) }
    if (maybeExisting != null) {
      onOpen(maybeExisting)
    } else {
      maybePendingDm = users
      viewModel.act(if (users.size == 1) ChatActions.openDm(users.single()) else ChatActions.openGroupDm(users))
    }
  }
  Scaffold(
    contentWindowInsets = ScreenInsets,
    topBar = { ChromeTopBar(title = stringResource(R.string.browse_title), maybeOnBack = onBack) },
    snackbarHost = { SnackbarHost(snackbars) },
    bottomBar = {
      if (isShowingPeople && picked.isNotEmpty()) {
        Button(
          onClick = startConversation,
          modifier = Modifier.fillMaxWidth().navigationBarsPadding().padding(16.dp),
        ) { Text(stringResource(if (picked.size == 1) R.string.dm_start else R.string.dm_start_group)) }
      }
    },
  ) { padding ->
    Column(Modifier.fillMaxSize().padding(padding)) {
      if (!isGuest) {
        PrimaryTabRow(selectedTabIndex = if (showPeople) 1 else 0) {
          Tab(selected = !showPeople, onClick = { showPeople = false }, text = { Text(stringResource(R.string.channels)) })
          Tab(selected = showPeople, onClick = { showPeople = true }, text = { Text(stringResource(R.string.people)) })
        }
      }
      val people = snapshot?.users.orEmpty().filter { it.id != snapshot?.me?.id && !it.isBot }
      val visibleChannels = channels.filter { !it.isArchived }
      if (isShowingPeople && snapshot != null && people.isEmpty()) {
        EmptyHint(R.string.browse_no_people)
      } else if (isShowingPeople) {
        Text(
          stringResource(R.string.dm_pick_hint),
          style = MaterialTheme.typography.bodySmall,
          color = MaterialTheme.colorScheme.onSurfaceVariant,
          modifier = Modifier.padding(start = 16.dp, end = 16.dp, top = 12.dp),
        )
        LazyColumn(Modifier.fillMaxSize()) {
          items(people, key = { it.id }) { user ->
            val isPicked = user.id in picked
            Row(
              Modifier
                .fillMaxWidth()
                .toggleable(value = isPicked, role = Role.Checkbox) { picked = toggled(picked, user.id) }
                .padding(16.dp),
              verticalAlignment = Alignment.CenterVertically,
            ) {
              Checkbox(checked = isPicked, onCheckedChange = null, modifier = Modifier.padding(end = 12.dp))
              Column(Modifier.weight(1f)) {
                Text(user.displayName)
                Text(
                  listOfNotNull("@" + user.username, user.maybeCustomStatus?.let { it.emoji + " " + it.text }).joinToString(" · "),
                  style = MaterialTheme.typography.bodySmall,
                  color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
              }
              PresenceDot(user.presence)
            }
            HorizontalDivider()
          }
        }
      } else {
        if (spaces.size > 1) {
          PrimaryTabRow(selectedTabIndex = spaceIndex.coerceAtMost(spaces.lastIndex)) {
            spaces.forEachIndexed { index, space ->
              Tab(selected = index == spaceIndex, onClick = { spaceIndex = index }, text = { Text(space.name) })
            }
          }
        }
        if (maybeSpace?.canCreateChannels == true) {
          TextButton(onClick = { isCreating = true }, modifier = Modifier.padding(horizontal = 8.dp)) {
            Text(stringResource(R.string.channel_create))
          }
        }
        if (snapshot != null && visibleChannels.isEmpty()) {
          EmptyHint(if (maybeSpace == null) R.string.browse_no_spaces else R.string.browse_no_channels)
        }
        LazyColumn(Modifier.fillMaxSize()) {
          items(visibleChannels, key = { it.conversation }) { channel ->
            val maybeJoin = BrowseJoin.of(channel.join)
            Row(
              Modifier.fillMaxWidth().clickable(enabled = channel.isMember) { onOpen(channel.conversation) }.padding(16.dp),
              verticalAlignment = Alignment.CenterVertically,
            ) {
              Column(Modifier.weight(1f)) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                  if (maybeJoin != BrowseJoin.Open) {
                    Icon(
                      Icons.Filled.Lock,
                      contentDescription = stringResource(R.string.browse_private),
                      modifier = Modifier.padding(end = 4.dp).size(14.dp),
                      tint = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                  }
                  val marker = if (maybeJoin == BrowseJoin.Open) "# " else ""
                  Text(marker + channel.name + if (channel.hasExternalPeople) " · " + stringResource(R.string.external_people) else "")
                }
                if (!channel.isMember && maybeJoin == BrowseJoin.Requested) {
                  Text(
                    stringResource(R.string.browse_join_requested_hint),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                  )
                } else if (channel.topic.isNotBlank()) {
                  Text(channel.topic, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
              }
              if (channel.isMember) {
                Text(stringResource(R.string.channel_member), style = MaterialTheme.typography.labelSmall)
              } else {
                JoinControl(
                  maybeJoin = maybeJoin,
                  isPending = channel.conversation in pendingJoins,
                  onJoin = { viewModel.act(ChatActions.joinChannel(channel.conversation)) },
                  onAsk = { viewModel.joinRequests.ask(channel.conversation) },
                  onWithdraw = { viewModel.joinRequests.withdraw(channel.conversation) },
                )
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
      label = R.string.channel_name_label,
      whenEmpty = R.string.channel_name_missing,
      initial = "",
      onConfirm = { name ->
        viewModel.tryAct(ChatActions.createChannel(maybeSpace.id, name.trim())).also { if (it == null) isCreating = false }
      },
      onDismiss = { isCreating = false },
    )
  }
}

/**
 * How the user gets into a channel they are not in: join a public one; ask the owners of a private
 * one (admin.md §2.4.1), then "Requested" with "Withdraw". Joining as an admin without asking
 * stays on desktop and web.
 */
@Composable
private fun JoinControl(
  maybeJoin: BrowseJoin?,
  isPending: Boolean,
  onJoin: () -> Unit,
  onAsk: () -> Unit,
  onWithdraw: () -> Unit,
) {
  when (maybeJoin) {
    BrowseJoin.Open -> OutlinedButton(onClick = onJoin) { Text(stringResource(R.string.channel_join)) }
    BrowseJoin.NeedsApproval -> OutlinedButton(onClick = onAsk, enabled = !isPending) {
      Text(stringResource(R.string.browse_join_ask))
    }
    BrowseJoin.Requested -> Column(horizontalAlignment = Alignment.End) {
      Text(
        stringResource(R.string.browse_join_requested),
        style = MaterialTheme.typography.labelSmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
      )
      TextButton(onClick = onWithdraw, enabled = !isPending) { Text(stringResource(R.string.browse_join_withdraw)) }
    }
    BrowseJoin.AsAdmin -> Text(
      stringResource(R.string.browse_join_admin_elsewhere),
      style = MaterialTheme.typography.labelSmall,
      color = MaterialTheme.colorScheme.onSurfaceVariant,
      textAlign = TextAlign.End,
      modifier = Modifier.widthIn(max = 140.dp),
    )
    null -> Unit
  }
}

/** Why a list is empty and what helps, instead of a blank screen. */
@Composable
private fun EmptyHint(@StringRes text: Int) {
  Text(
    stringResource(text),
    style = MaterialTheme.typography.bodyMedium,
    color = MaterialTheme.colorScheme.onSurfaceVariant,
    textAlign = TextAlign.Center,
    modifier = Modifier.fillMaxWidth().padding(32.dp),
  )
}

/**
 * The DM (one person) or group DM (several) with exactly these people and the user, if there is
 * one.
 */
fun directWith(snapshot: ChatSnapshot, users: List<String>): String? {
  val wanted = users.toSet() - snapshot.me.id
  val kind = if (wanted.size == 1) "Direct" else "Group"
  return snapshot.conversations.firstOrNull { it.kind == kind && it.memberIds.toSet() - snapshot.me.id == wanted }?.id
}

/** A small coloured dot for a presence state as the core reports it. */
@Composable
fun PresenceDot(presence: String) {
  val color = when (presence) {
    "Online" -> Color(0xFF34C759)
    "Away" -> Color(0xFFFF9F0A)
    "DoNotDisturb" -> Color(0xFFFF3B30)
    else -> MaterialTheme.colorScheme.outline
  }
  Box(Modifier.size(10.dp).background(color, CircleShape))
}
