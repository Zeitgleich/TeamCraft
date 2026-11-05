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
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.CallHistoryItem
import de.zeitgleich.teamcraft.core.ChatActions
import de.zeitgleich.teamcraft.core.ChatQueries
import de.zeitgleich.teamcraft.core.ChatSnapshot
import de.zeitgleich.teamcraft.core.Meeting
import de.zeitgleich.teamcraft.core.SavedItem
import de.zeitgleich.teamcraft.core.Space
import de.zeitgleich.teamcraft.core.SpaceRoster
import de.zeitgleich.teamcraft.ui.MainViewModel
import de.zeitgleich.teamcraft.ui.SpacePicture
import kotlinx.coroutines.delay

/** How long a space's member list may load before the user is offered another try. */
private const val ROSTER_DEADLINE_MS = 15_000L

/** The views of one space, in the order of the sidebar on the desktop. */
enum class SpaceViewTab(@StringRes val label: Int) {
  People(R.string.space_tab_people),
  Calls(R.string.space_tab_calls),
  Meetings(R.string.space_tab_meetings),
  Saved(R.string.space_tab_saved),
  Files(R.string.space_tab_files),
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
fun SpaceViewsScreen(viewModel: MainViewModel, initialSpaceId: String, onOpen: (String) -> Unit, onBack: () -> Unit) {
  val changes by viewModel.changeCount.collectAsState()
  var spaceId by rememberSaveable { mutableStateOf(initialSpaceId) }
  var tab by rememberSaveable { mutableStateOf(SpaceViewTab.People) }
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
        SpaceViewTab.Calls -> SpaceCalls(viewModel, spaceId, snapshot)
        SpaceViewTab.Meetings -> SpaceMeetings(viewModel, spaceId)
        SpaceViewTab.Saved -> SpaceSaved(viewModel, spaceId, snapshot, onOpen)
        SpaceViewTab.Files -> SpaceFiles(viewModel, spaceId)
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
        Column(Modifier.padding(16.dp)) {
          EmptyHint(stringResource(R.string.space_people_failed))
          TextButton(onClick = { attempt += 1 }) { Text(stringResource(R.string.retry)) }
        }
      }
    roster.state == "hidden" -> EmptyHint(stringResource(R.string.space_people_hidden))
    else -> {
      val people = peopleOfRoster(roster)
      if (people.isEmpty()) EmptyHint(stringResource(R.string.space_people_empty))
      LazyColumn(Modifier.fillMaxSize()) {
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
  val changes by viewModel.changeCount.collectAsState()
  val context = LocalContext.current
  var maybeCalls by remember(spaceId) { mutableStateOf<List<CallHistoryItem>?>(null) }
  LaunchedEffect(Unit) { viewModel.act(ChatActions.refreshCallHistory()) }
  LaunchedEffect(changes, spaceId) { maybeCalls = viewModel.readChat { query(ChatQueries.spaceCallHistory(spaceId)).calls } }
  val calls = maybeCalls ?: return
  if (calls.isEmpty()) EmptyHint(stringResource(R.string.space_calls_empty))
  LazyColumn(Modifier.fillMaxSize()) {
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
  val changes by viewModel.changeCount.collectAsState()
  var maybeMeetings by remember { mutableStateOf<List<Meeting>?>(null) }
  LaunchedEffect(changes) { maybeMeetings = viewModel.readChat { meetings() } }
  val meetings = meetingsOfSpace(maybeMeetings ?: return, spaceId)
  if (meetings.isEmpty()) EmptyHint(stringResource(R.string.space_meetings_empty))
  LazyColumn(Modifier.fillMaxSize()) {
    items(meetings, key = { "${it.id}-${it.startsAt}" }) { meeting ->
      MeetingRow(viewModel, meeting)
      HorizontalDivider()
    }
  }
}

@Composable
private fun SpaceSaved(viewModel: MainViewModel, spaceId: String, snapshot: ChatSnapshot?, onOpen: (String) -> Unit) {
  val changes by viewModel.changeCount.collectAsState()
  var maybeSaved by remember(spaceId) { mutableStateOf<List<SavedItem>?>(null) }
  LaunchedEffect(changes, spaceId) { maybeSaved = viewModel.readChat { query(ChatQueries.spaceSavedItems(spaceId)).items } }
  val saved = maybeSaved ?: return
  if (saved.isEmpty()) EmptyHint(stringResource(R.string.space_saved_empty))
  LazyColumn(Modifier.fillMaxSize()) {
    items(saved, key = { it.message.id }) { item ->
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
  val maybeFiles = rememberFiles(viewModel, ChatQueries.spaceFiles(spaceId))
  if (maybeFiles?.isEmpty() == true) EmptyHint(stringResource(R.string.space_files_empty))
  FilesGrid(viewModel, maybeFiles.orEmpty(), Modifier.fillMaxSize())
}
