package de.zeitgleich.teamcraft.ui.screens

import android.text.format.DateUtils
import androidx.annotation.StringRes
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.ArrowDropDown
import androidx.compose.material.icons.filled.Call
import androidx.compose.material.icons.filled.DateRange
import androidx.compose.material.icons.filled.Email
import androidx.compose.material.icons.filled.Person
import androidx.compose.material3.Button
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.PrimaryScrollableTabRow
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Tab
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.CallHistoryItem
import de.zeitgleich.teamcraft.core.ChatActions
import de.zeitgleich.teamcraft.core.ChatQueries
import de.zeitgleich.teamcraft.core.ChatSnapshot
import de.zeitgleich.teamcraft.core.ListLoad
import de.zeitgleich.teamcraft.core.LoadFailure
import de.zeitgleich.teamcraft.core.LoadingList
import de.zeitgleich.teamcraft.core.Meeting
import de.zeitgleich.teamcraft.core.Space
import de.zeitgleich.teamcraft.core.SpaceRoster
import de.zeitgleich.teamcraft.ui.ListPhase
import de.zeitgleich.teamcraft.ui.MainViewModel
import de.zeitgleich.teamcraft.ui.Read
import de.zeitgleich.teamcraft.ui.listPhase
import de.zeitgleich.teamcraft.ui.spaceDirectMessages
import de.zeitgleich.teamcraft.ui.SpacePicture
import kotlinx.coroutines.delay
import uniffi.teamcraft_ffi.rowBadge

/** How long a space's member list may load before the user is offered another try. */
private const val ROSTER_DEADLINE_MS = 15_000L

/** The views of one space, in the order of the sidebar on the desktop. */
enum class SpaceViewTab(@StringRes val label: Int) {
  People(R.string.space_tab_people),
  DirectMessages(R.string.space_tab_direct_messages),
  Calls(R.string.space_tab_calls),
  Meetings(R.string.space_tab_meetings),
  Saved(R.string.space_tab_saved),
  Files(R.string.space_tab_files),
}

/** A view's icon, as the space menu and Home's header show it (#1526). */
@Composable
fun SpaceViewIcon(tab: SpaceViewTab) {
  when (tab) {
    SpaceViewTab.People -> Icon(Icons.Filled.Person, contentDescription = null)
    SpaceViewTab.DirectMessages -> Icon(Icons.Filled.Email, contentDescription = null)
    SpaceViewTab.Calls -> Icon(Icons.Filled.Call, contentDescription = null)
    SpaceViewTab.Meetings -> Icon(Icons.Filled.DateRange, contentDescription = null)
    SpaceViewTab.Saved -> Icon(painterResource(R.drawable.ic_bookmark), contentDescription = null)
    SpaceViewTab.Files -> Icon(painterResource(R.drawable.ic_description), contentDescription = null)
  }
}

/** The upcoming meetings of a space, soonest first; a meeting belongs to the space it was scheduled in. */
fun meetingsOfSpace(meetings: List<Meeting>, spaceId: String): List<Meeting> =
  upcomingMeetings(meetings).filter { it.maybeSpaceId == spaceId }

/** The roster as a list: people first by name, integrations' bot accounts left out. */
fun peopleOfRoster(roster: SpaceRoster) = roster.members.filterNot { it.isBot }.sortedBy { it.displayName.lowercase() }

/**
 * People, calls, meetings, saved messages and files of one space (#954). Home's entries stay global;
 * this screen is what "inside a space" means on a phone, where the channels of all spaces share one
 * list and none is selected. Everything content is filtered by the core on this device.
 */
@Composable
fun SpaceViewsScreen(
  viewModel: MainViewModel,
  initialSpaceId: String,
  onOpen: (String) -> Unit,
  onBack: () -> Unit,
  initialTab: SpaceViewTab = SpaceViewTab.People,
) {
  val changes by viewModel.changeCount.collectAsState()
  var spaceId by rememberSaveable { mutableStateOf(initialSpaceId) }
  var tab by rememberSaveable { mutableStateOf(initialTab) }
  var snapshot by remember { mutableStateOf<ChatSnapshot?>(null) }
  LaunchedEffect(changes) { snapshot = viewModel.readChat { snapshot() } }
  val spaces = snapshot?.spaces.orEmpty()
  Scaffold(
    topBar = {
      ChromeTopBar(
        title = spaces.firstOrNull { it.id == spaceId }?.name.orEmpty(),
        maybeIcon = spaces.firstOrNull { it.id == spaceId }?.maybeImageVersion?.let { version -> { SpacePicture(spaceId, version, size = 28.dp) } },
        maybeOnBack = onBack,
        actions = { if (spaces.size > 1) SpaceSwitcher(spaces, onSelect = { spaceId = it }) },
      )
    },
    contentWindowInsets = ScreenInsets,
  ) { padding ->
    Column(Modifier.fillMaxSize().padding(padding)) {
      PrimaryScrollableTabRow(selectedTabIndex = tab.ordinal) {
        for (entry in SpaceViewTab.entries) {
          Tab(selected = tab == entry, onClick = { tab = entry }, text = { Text(stringResource(entry.label)) })
        }
      }
      when (tab) {
        SpaceViewTab.People -> SpacePeople(viewModel, spaceId)
        SpaceViewTab.DirectMessages ->
          SpaceDirectMessages(viewModel, spaces.firstOrNull { it.id == spaceId }?.directConversations.orEmpty(), onOpen)
        SpaceViewTab.Calls -> SpaceCalls(viewModel, spaceId, snapshot)
        SpaceViewTab.Meetings -> SpaceMeetings(viewModel, spaceId)
        SpaceViewTab.Saved -> SpaceSaved(viewModel, spaceId, snapshot, onOpen)
        SpaceViewTab.Files -> SpaceFiles(viewModel, spaceId)
      }
    }
  }
}

/**
 * The DMs and group DMs with people of the space (#701), the most recent first. They are Home's
 * conversations, shown here too; opening one opens it as from Home.
 */
@Composable
private fun SpaceDirectMessages(viewModel: MainViewModel, ids: List<String>, onOpen: (String) -> Unit) {
  val kept by viewModel.chatList.collectAsState()
  val lastActivity = kept.previews.mapValues { it.value.preview.sentAt }
  val conversations = spaceDirectMessages(kept.maybeConversations.orEmpty(), ids, lastActivity)
  LazyColumn(Modifier.fillMaxSize()) {
    if (conversations.isEmpty()) item { EmptyHint(stringResource(R.string.space_direct_messages_empty)) }
    items(conversations, key = { it.id }) { conversation ->
      Row(
        Modifier.fillMaxWidth().clickable { onOpen(conversation.id) }.padding(horizontal = 16.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
      ) {
        ConversationAvatar(conversation)
        Text(
          conversation.name,
          modifier = Modifier.weight(1f).padding(start = 12.dp),
          style = MaterialTheme.typography.bodyLarge,
          fontWeight = if (conversation.unreadCount > 0u || conversation.mentionCount > 0u) FontWeight.SemiBold else null,
          maxLines = 1,
          overflow = TextOverflow.Ellipsis,
        )
        rowBadge(conversation.unreadCount, conversation.mentionCount, conversation.isMuted)?.let { badge ->
          RowCount(badge, conversation.isMuted)
        }
      }
    }
  }
}

/** Picks another space: the open view stays and shows the new space at once. */
@Composable
private fun SpaceSwitcher(spaces: List<Space>, onSelect: (String) -> Unit) {
  var isOpen by remember { mutableStateOf(false) }
  Box {
    IconButton(onClick = { isOpen = true }) {
      Icon(Icons.Filled.ArrowDropDown, contentDescription = stringResource(R.string.space_views_switch))
    }
    DropdownMenu(expanded = isOpen, onDismissRequest = { isOpen = false }) {
      for (space in spaces) {
        DropdownMenuItem(
          text = { Text(space.name) },
          onClick = {
            isOpen = false
            onSelect(space.id)
          },
        )
      }
    }
  }
}

@Composable
private fun SpacePeople(viewModel: MainViewModel, spaceId: String) {
  val changes by viewModel.changeCount.collectAsState()
  var maybeRoster by remember(spaceId) { mutableStateOf<SpaceRoster?>(null) }
  var attempt by remember(spaceId) { mutableStateOf(0) }
  var isOverdue by remember(spaceId) { mutableStateOf(false) }
  LaunchedEffect(changes, spaceId) { maybeRoster = viewModel.readChat { query(ChatQueries.spaceMembers(spaceId)).roster } }
  // What this device has shows at once; the fresh list follows as a store change.
  LaunchedEffect(spaceId, attempt) {
    isOverdue = false
    viewModel.act(ChatActions.refreshSpaceRoster(spaceId))
    delay(ROSTER_DEADLINE_MS)
    isOverdue = true
  }
  val roster = maybeRoster
  when {
    roster == null || roster.state == "loading" ->
      if (isOverdue) {
        ListFailed(ListPhase.Failed(LoadFailure.TIMED_OUT), onRetry = { attempt += 1 })
      } else {
        ListSkeleton()
      }
    roster.state == "hidden" -> EmptyHint(stringResource(R.string.space_people_hidden))
    else -> {
      val people = peopleOfRoster(roster)
      LazyColumn(Modifier.fillMaxSize()) {
        if (people.isEmpty()) item { EmptyHint(stringResource(R.string.space_people_empty)) }
        items(people, key = { it.user }) { member ->
          Column(Modifier.fillMaxWidth().padding(16.dp)) {
            Text(member.displayName)
            if (member.isGuest) {
              Text(
                stringResource(R.string.space_person_guest),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
              )
            }
          }
          HorizontalDivider()
        }
      }
    }
  }
}

@Composable
private fun SpaceCalls(viewModel: MainViewModel, spaceId: String, snapshot: ChatSnapshot?) {
  val context = LocalContext.current
  val read = rememberRead(viewModel, spaceId) { query(ChatQueries.spaceCallHistory(spaceId)).calls }
  // The call history comes from the server: the core says whether that answer is here (#297).
  val load = rememberRead(viewModel) { query(ChatQueries.listLoad(LoadingList.CALL_HISTORY)).load?.toLoad() }
  LaunchedEffect(Unit) { viewModel.act(ChatActions.refreshCallHistory()) }
  val calls = read.maybeValue.orEmpty()
  val phase = when (read.read) {
    is Read.Loaded -> listPhase(load.maybeValue ?: ListLoad.Ready, calls.isEmpty())
    else -> read.phase { it }
  }
  val retry = {
    viewModel.act(ChatActions.retryList(LoadingList.CALL_HISTORY))
    load.retry()
    read.retry()
  }
  LazyColumn(Modifier.fillMaxSize()) {
    placeholderItem(phase, retry) { EmptyHint(stringResource(R.string.space_calls_empty)) }
    items(calls, key = { it.id }) { call ->
      val name = snapshot?.conversation(call.conversation)?.name.orEmpty()
      Row(Modifier.fillMaxWidth().padding(16.dp), verticalAlignment = Alignment.CenterVertically) {
        Column(Modifier.weight(1f)) {
          Text(name, style = MaterialTheme.typography.titleMedium)
          Text(
            stringResource(callDirectionLabel(call.direction)) + " · " + DateUtils.getRelativeTimeSpanString(context, call.startedAt),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
          )
        }
        Button(onClick = { viewModel.startCall(call.conversation, name) }) { Text(stringResource(R.string.space_call_back)) }
      }
      HorizontalDivider()
    }
  }
}

@StringRes
private fun callDirectionLabel(direction: String): Int = when (direction) {
  "missed" -> R.string.space_call_missed
  "outgoing" -> R.string.space_call_outgoing
  else -> R.string.space_call_incoming
}

@Composable
private fun SpaceMeetings(viewModel: MainViewModel, spaceId: String) {
  val read = rememberRead(viewModel) { meetings() }
  val meetings = meetingsOfSpace(read.maybeValue.orEmpty(), spaceId)
  LazyColumn(Modifier.fillMaxSize()) {
    placeholderItem(read.phase { meetingsOfSpace(it, spaceId) }, read.retry) { EmptyHint(stringResource(R.string.space_meetings_empty)) }
    items(meetings, key = { "${it.id}-${it.startsAt}" }) { meeting ->
      MeetingRow(viewModel, meeting)
      HorizontalDivider()
    }
  }
}

@Composable
private fun SpaceSaved(viewModel: MainViewModel, spaceId: String, snapshot: ChatSnapshot?, onOpen: (String) -> Unit) {
  val saved = rememberRead(viewModel, spaceId) { query(ChatQueries.spaceSavedItems(spaceId)).items }
  LazyColumn(Modifier.fillMaxSize()) {
    placeholderItem(saved.phase { it }, saved.retry) { EmptyHint(stringResource(R.string.space_saved_empty)) }
    items(saved.maybeValue.orEmpty(), key = { it.message.id }) { item ->
      Column(Modifier.fillMaxWidth().clickable { onOpen(item.message.conversation) }.padding(16.dp)) {
        Text(item.conversationName, style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.primary)
        MessageLine(snapshot, item.message)
      }
      HorizontalDivider()
    }
  }
}

@Composable
private fun SpaceFiles(viewModel: MainViewModel, spaceId: String) {
  val files = rememberFiles(viewModel, ChatQueries.spaceFiles(spaceId))
  FilesContent(viewModel, files, R.string.space_files_empty, Modifier.fillMaxSize())
}
