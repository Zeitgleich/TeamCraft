package de.zeitgleich.teamcraft.ui.screens

import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.consumeWindowInsets
import androidx.annotation.StringRes
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.ArrowDropDown
import androidx.compose.material.icons.filled.Lock
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SecondaryScrollableTabRow
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.Tab
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.Stable
import androidx.compose.runtime.State
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.ChatActions
import de.zeitgleich.teamcraft.core.ChatClient
import de.zeitgleich.teamcraft.core.ChatSnapshot
import de.zeitgleich.teamcraft.core.LoadFailure
import de.zeitgleich.teamcraft.core.Space
import de.zeitgleich.teamcraft.ui.ActionAnswer
import de.zeitgleich.teamcraft.ui.ListPhase
import de.zeitgleich.teamcraft.ui.MainViewModel
import de.zeitgleich.teamcraft.ui.PageAnswer
import de.zeitgleich.teamcraft.ui.UserAvatar
import de.zeitgleich.teamcraft.ui.forms.CreateResult
import de.zeitgleich.teamcraft.ui.theme.Spacing
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import uniffi.teamcraft_ffi.DirectoryChannelItem
import uniffi.teamcraft_ffi.DirectoryFilterChoices
import uniffi.teamcraft_ffi.DirectoryInvitationItem
import uniffi.teamcraft_ffi.DirectoryPeopleKind
import uniffi.teamcraft_ffi.DirectoryPeopleRequest
import uniffi.teamcraft_ffi.DirectoryPersonItem
import uniffi.teamcraft_ffi.InvitationOutcome
import java.text.DateFormat
import java.util.Date

/** The directory is asked once typing paused this long. */
private const val SEARCH_PAUSE_MS = 250L

/**
 * The directory's tabs (#1337), as on the web. Guests have no directory beyond the people they
 * share a conversation with (docs/admin.md); Invitations is for those who may invite.
 */
enum class DirectoryTab(@StringRes val label: Int) {
  People(R.string.directory_people),
  Channels(R.string.directory_channels),
  External(R.string.directory_external),
  Invitations(R.string.directory_invitations),
  ;

  companion object {
    fun offered(isGuest: Boolean, canInvite: Boolean): List<DirectoryTab> = when {
      isGuest -> listOf(People)
      canInvite -> entries
      else -> listOf(People, Channels, External)
    }
  }
}

/** The spaces the user may invite people to: as members, as guests, or both. */
fun invitingSpaces(spaces: List<Space>): List<Space> = spaces.filter { it.canAddMembers || it.canInviteGuests }

/** The directory across all the user's spaces, a screen of its own (Home's "New" menu). */
@Composable
fun DirectoryScreen(viewModel: MainViewModel, onOpen: (String) -> Unit, onBack: () -> Unit) {
  val snackbars = rememberFailureSnackbars(viewModel)
  Scaffold(
    contentWindowInsets = ScreenInsets,
    topBar = { ChromeTopBar(title = stringResource(R.string.directory_title), maybeOnBack = onBack) },
    snackbarHost = { SnackbarHost(snackbars) },
  ) { padding ->
    DirectoryContent(
      viewModel,
      maybeSpaceId = null,
      onOpen = onOpen,
      modifier = Modifier.fillMaxSize().padding(padding.withoutBottom()).consumeWindowInsets(padding),
      contentPadding = padding.bottomOnly(),
    )
  }
}

/**
 * People, channels, external people and invitations, each paged from the server (#1337). With
 * [maybeSpaceId] every list shows that space only (a space's People view); without, all the user's
 * spaces. The lists take [contentPadding] (the bottom inset): they scroll behind the navigation bar.
 */
@Composable
fun DirectoryContent(
  viewModel: MainViewModel,
  maybeSpaceId: String?,
  onOpen: (String) -> Unit,
  modifier: Modifier = Modifier,
  contentPadding: PaddingValues = PaddingValues(),
) {
  val changes by viewModel.changeCount.collectAsState()
  var snapshot by remember { mutableStateOf<ChatSnapshot?>(null) }
  LaunchedEffect(changes) { viewModel.readChat { snapshot() }?.let { snapshot = it } }
  val isGuest = snapshot?.me?.isGuest == true
  val spaces = snapshot?.spaces.orEmpty().filter { maybeSpaceId == null || it.id == maybeSpaceId }
  val inviting = invitingSpaces(spaces)
  val tabs = DirectoryTab.offered(isGuest, inviting.isNotEmpty())
  var chosen by rememberSaveable { mutableStateOf(DirectoryTab.People) }
  val tab = chosen.takeIf { it in tabs } ?: DirectoryTab.People
  var isInviting by rememberSaveable { mutableStateOf(false) }
  // An invitation sent from the sheet shows in the list once the sheet closed.
  var invitationsGeneration by rememberSaveable { mutableStateOf(0) }
  val spaceId = maybeSpaceId.orEmpty()
  Column(modifier) {
    if (tabs.size > 1) {
      SecondaryScrollableTabRow(selectedTabIndex = tabs.indexOf(tab), edgePadding = Spacing.s) {
        for (entry in tabs) {
          Tab(selected = entry == tab, onClick = { chosen = entry }, text = { Text(stringResource(entry.label)) })
        }
      }
    }
    val onInvite = { isInviting = true }.takeIf { inviting.isNotEmpty() }
    when (tab) {
      DirectoryTab.People ->
        if (isGuest && maybeSpaceId != null) {
          // A guest sees no roster of a space, only the people they share a conversation with.
          DirectoryHint(R.string.space_people_hidden)
        } else {
          PeopleList(viewModel, spaceId, if (isGuest) DirectoryPeopleKind.EVERYBODY else DirectoryPeopleKind.MEMBERS, snapshot, onOpen, onInvite, contentPadding)
        }
      DirectoryTab.External -> PeopleList(viewModel, spaceId, DirectoryPeopleKind.GUESTS, snapshot, onOpen, onInvite, contentPadding)
      DirectoryTab.Channels -> ChannelList(viewModel, spaceId, snapshot, onOpen, contentPadding)
      DirectoryTab.Invitations -> InvitationList(viewModel, spaceId, invitationsGeneration, onInvite, contentPadding)
    }
  }
  if (isInviting && inviting.isNotEmpty()) {
    InvitePeopleSheet(
      viewModel = viewModel,
      spaces = inviting,
      onSent = {
        isInviting = false
        invitationsGeneration += 1
      },
      onDismiss = { isInviting = false },
    )
  }
}

// People

@Composable
private fun PeopleList(
  viewModel: MainViewModel,
  spaceId: String,
  kind: DirectoryPeopleKind,
  snapshot: ChatSnapshot?,
  onOpen: (String) -> Unit,
  maybeOnInvite: (() -> Unit)?,
  contentPadding: PaddingValues,
) {
  val scope = rememberCoroutineScope()
  var text by rememberSaveable(kind) { mutableStateOf("") }
  val query by debounced(text)
  var organization by rememberSaveable(kind, spaceId) { mutableStateOf("") }
  var department by rememberSaveable(kind, spaceId) { mutableStateOf("") }
  var title by rememberSaveable(kind, spaceId) { mutableStateOf("") }
  var choices by remember(kind, spaceId) { mutableStateOf(DirectoryFilterChoices(emptyList(), emptyList(), emptyList())) }
  // Guests have no filters: the server answers them empty.
  LaunchedEffect(kind, spaceId) {
    if (kind == DirectoryPeopleKind.EVERYBODY) return@LaunchedEffect
    (viewModel.fetchPage { directoryFilters(kind, spaceId) } as? PageAnswer.Loaded)?.let { choices = it.value }
  }
  val pager = rememberPager(kind, spaceId, query, organization, department, title, idOf = DirectoryPersonItem::userId) { after ->
    viewModel.fetchPage {
      val page = directoryPeople(DirectoryPeopleRequest(query, kind, organization, department, title, spaceId, after))
      Page(page.people, page.nextAfter)
    }
  }
  var isOpening by remember { mutableStateOf(false) }
  val me = snapshot?.me?.id
  val message: (DirectoryPersonItem) -> Unit = message@{ person ->
    if (isOpening) return@message
    snapshot?.let { directWith(it, listOf(person.userId)) }?.let { existing ->
      onOpen(existing)
      return@message
    }
    isOpening = true
    scope.launch {
      when (val result = viewModel.tryCreate { openDirect(listOf(person.userId)) }) {
        is CreateResult.Created -> onOpen(result.id)
        CreateResult.NameRefused -> viewModel.showFailure(R.string.error_generic)
        is CreateResult.Failed -> viewModel.showFailure(result.text)
      }
      isOpening = false
    }
  }
  Column(Modifier.fillMaxSize()) {
    ListTools(text, { text = it }, R.string.directory_people_filter, maybeOnInvite)
    if (choices.organizations.isNotEmpty() || choices.departments.isNotEmpty() || choices.titles.isNotEmpty()) {
      Row(
        Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()).padding(horizontal = Spacing.l),
        horizontalArrangement = Arrangement.spacedBy(Spacing.s),
      ) {
        FilterMenu(R.string.directory_filter_organization, choices.organizations, organization) { organization = it }
        FilterMenu(R.string.directory_filter_department, choices.departments, department) { department = it }
        FilterMenu(R.string.directory_filter_title, choices.titles, title) { title = it }
      }
    }
    LazyColumn(Modifier.fillMaxSize(), contentPadding = contentPadding) {
      items(pager.rows, key = { it.userId }) { person ->
        PersonRow(person, onMessage = { message(person) }.takeIf { person.userId != me })
        HorizontalDivider()
      }
      pagerFooter(pager) {
        DirectoryHint(if (kind == DirectoryPeopleKind.GUESTS) R.string.directory_external_empty else R.string.directory_people_empty)
      }
    }
  }
}

/** A profile filter: a chip that opens its values; "Any" clears it. Hidden while it offers nothing. */
@Composable
private fun FilterMenu(@StringRes label: Int, values: List<String>, chosen: String, onChoose: (String) -> Unit) {
  if (values.isEmpty() && chosen.isEmpty()) return
  var isOpen by remember { mutableStateOf(false) }
  Box {
    FilterChip(
      selected = chosen.isNotEmpty(),
      onClick = { isOpen = true },
      label = {
        Text(
          if (chosen.isEmpty()) stringResource(label) else stringResource(R.string.directory_filter_chosen, stringResource(label), chosen),
          maxLines = 1,
          overflow = TextOverflow.Ellipsis,
        )
      },
      trailingIcon = { Icon(Icons.Filled.ArrowDropDown, contentDescription = null) },
    )
    DropdownMenu(expanded = isOpen, onDismissRequest = { isOpen = false }) {
      DropdownMenuItem(
        text = { Text(stringResource(R.string.directory_filter_any)) },
        onClick = {
          isOpen = false
          onChoose("")
        },
      )
      for (value in values) {
        DropdownMenuItem(
          text = { Text(value) },
          onClick = {
            isOpen = false
            onChoose(value)
          },
        )
      }
    }
  }
}

@Composable
private fun PersonRow(person: DirectoryPersonItem, onMessage: (() -> Unit)?) {
  val messageLabel = stringResource(R.string.directory_message, person.displayName)
  Row(
    Modifier
      .fillMaxWidth()
      .then(if (onMessage != null) Modifier.clickable(role = Role.Button, onClickLabel = messageLabel, onClick = onMessage) else Modifier)
      .padding(horizontal = Spacing.l, vertical = Spacing.s),
    verticalAlignment = Alignment.CenterVertically,
  ) {
    Box {
      UserAvatar(person.userId, Modifier.size(40.dp)) {
        Box(Modifier.size(40.dp).background(MaterialTheme.colorScheme.secondaryContainer, CircleShape), contentAlignment = Alignment.Center) {
          Text(
            person.displayName.firstOrNull()?.uppercase().orEmpty(),
            color = MaterialTheme.colorScheme.onSecondaryContainer,
            style = MaterialTheme.typography.titleMedium,
          )
        }
      }
      Box(
        Modifier.align(Alignment.BottomEnd).size(14.dp).background(MaterialTheme.colorScheme.background, CircleShape),
        contentAlignment = Alignment.Center,
      ) { PresenceDot(corePresence(person.presence)) }
    }
    Column(Modifier.weight(1f).padding(start = Spacing.m)) {
      Text(person.displayName, maxLines = 1, overflow = TextOverflow.Ellipsis)
      val detail = listOf(person.title, person.department, person.organization).filter { it.isNotBlank() }.joinToString(" · ")
      if (detail.isNotEmpty()) {
        Text(
          detail,
          style = MaterialTheme.typography.bodySmall,
          color = MaterialTheme.colorScheme.onSurfaceVariant,
          maxLines = 1,
          overflow = TextOverflow.Ellipsis,
        )
      }
      if (person.isAwaitingFirstSignIn) {
        Text(
          stringResource(R.string.directory_not_signed_in),
          style = MaterialTheme.typography.bodySmall,
          color = MaterialTheme.colorScheme.onSurfaceVariant,
          maxLines = 1,
        )
      }
    }
    if (person.isGuest) GuestBadge()
  }
}

/** The directory's presence (`online`, `dnd`, …) as [PresenceDot] takes it. */
internal fun corePresence(wire: String): String = when (wire) {
  "online" -> "Online"
  "busy" -> "Busy"
  "brb" -> "BeRightBack"
  "away" -> "Away"
  "dnd" -> "DoNotDisturb"
  else -> "Offline"
}

@Composable
private fun GuestBadge() {
  Text(
    stringResource(R.string.directory_guest),
    style = MaterialTheme.typography.labelSmall,
    color = MaterialTheme.colorScheme.onTertiaryContainer,
    modifier = Modifier
      .background(MaterialTheme.colorScheme.tertiaryContainer, CircleShape)
      .padding(horizontal = Spacing.s, vertical = 2.dp),
  )
}

// Channels

@Composable
private fun ChannelList(viewModel: MainViewModel, spaceId: String, snapshot: ChatSnapshot?, onOpen: (String) -> Unit, contentPadding: PaddingValues) {
  var text by rememberSaveable { mutableStateOf("") }
  val query by debounced(text)
  val pager = rememberPager(spaceId, query, idOf = DirectoryChannelItem::conversationId) { after ->
    viewModel.fetchPage {
      val page = directoryChannels(query, spaceId, after)
      Page(page.channels, page.nextAfter)
    }
  }
  // The answer is from when the page loaded; a join here shows as soon as the channel arrives.
  val joined = snapshot?.conversations.orEmpty().map { it.id }.toSet()
  var joining by remember { mutableStateOf(setOf<String>()) }
  Column(Modifier.fillMaxSize()) {
    ListTools(text, { text = it }, R.string.directory_channel_filter, maybeOnInvite = null)
    LazyColumn(Modifier.fillMaxSize(), contentPadding = contentPadding) {
      items(pager.rows, key = { it.conversationId }) { channel ->
        val isMember = channel.isMember || channel.conversationId in joined
        ChannelRow(
          channel = channel,
          isMember = isMember,
          isJoining = channel.conversationId in joining && !isMember,
          onOpen = { onOpen(channel.conversationId) },
          onJoin = {
            joining = joining + channel.conversationId
            viewModel.act(ChatActions.joinChannel(channel.conversationId))
          },
        )
        HorizontalDivider()
      }
      pagerFooter(pager) { DirectoryHint(R.string.directory_channels_empty) }
    }
  }
}

@Composable
private fun ChannelRow(channel: DirectoryChannelItem, isMember: Boolean, isJoining: Boolean, onOpen: () -> Unit, onJoin: () -> Unit) {
  Row(
    Modifier
      .fillMaxWidth()
      .clickable(enabled = isMember, role = Role.Button, onClick = onOpen)
      .padding(horizontal = Spacing.l, vertical = Spacing.m),
    verticalAlignment = Alignment.CenterVertically,
  ) {
    if (channel.isPrivate) {
      Icon(Icons.Filled.Lock, contentDescription = stringResource(R.string.directory_private), tint = MaterialTheme.colorScheme.onSurfaceVariant)
    } else {
      Icon(painterResource(R.drawable.ic_tag), contentDescription = null, tint = MaterialTheme.colorScheme.onSurfaceVariant)
    }
    Column(Modifier.weight(1f).padding(horizontal = Spacing.m)) {
      Text(channel.name, maxLines = 1, overflow = TextOverflow.Ellipsis)
      Text(
        channel.spaceName + " · " + pluralStringResource(R.plurals.directory_channel_members, channel.memberCount.toInt(), channel.memberCount.toInt()),
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        maxLines = 1,
        overflow = TextOverflow.Ellipsis,
      )
      if (channel.topic.isNotBlank()) {
        Text(
          channel.topic,
          style = MaterialTheme.typography.bodySmall,
          color = MaterialTheme.colorScheme.onSurfaceVariant,
          maxLines = 2,
          overflow = TextOverflow.Ellipsis,
        )
      }
    }
    when {
      isMember -> TextButton(onClick = onOpen) { Text(stringResource(R.string.directory_open)) }
      isJoining -> {
        val joiningLabel = stringResource(R.string.directory_joining)
        CircularProgressIndicator(Modifier.size(20.dp).semantics { contentDescription = joiningLabel }, strokeWidth = 2.dp)
      }
      else -> OutlinedButton(onClick = onJoin) { Text(stringResource(R.string.channel_join)) }
    }
  }
}

// Invitations

@Composable
private fun InvitationList(viewModel: MainViewModel, spaceId: String, generation: Int, maybeOnInvite: (() -> Unit)?, contentPadding: PaddingValues) {
  var text by rememberSaveable { mutableStateOf("") }
  val query by debounced(text)
  val pager = rememberPager(spaceId, query, generation, idOf = DirectoryInvitationItem::invitationId) { after ->
    viewModel.fetchPage {
      val page = directoryInvitations(query, spaceId, after)
      Page(page.invitations, page.nextAfter)
    }
  }
  Column(Modifier.fillMaxSize()) {
    ListTools(text, { text = it }, R.string.directory_invitation_filter, maybeOnInvite)
    LazyColumn(Modifier.fillMaxSize(), contentPadding = contentPadding) {
      items(pager.rows, key = { it.invitationId }) { invitation ->
        InvitationRow(viewModel, invitation, onGone = { pager.remove { it.invitationId == invitation.invitationId } })
        HorizontalDivider()
      }
      pagerFooter(pager) { DirectoryHint(R.string.directory_invitations_empty) }
    }
  }
}

/** What a row says after the user resent its invitation. */
private enum class ResendState { NONE, SENT, TOO_SOON }

@Composable
private fun InvitationRow(viewModel: MainViewModel, invitation: DirectoryInvitationItem, onGone: () -> Unit) {
  val scope = rememberCoroutineScope()
  var isBusy by remember { mutableStateOf(false) }
  var resend by remember { mutableStateOf(ResendState.NONE) }
  var isConfirming by remember { mutableStateOf(false) }
  fun act(call: suspend ChatClient.() -> InvitationOutcome, onDone: () -> Unit) {
    if (isBusy) return
    isBusy = true
    scope.launch {
      when (val answer = viewModel.tryDirectoryAction(call)) {
        is ActionAnswer.Answered -> when (answer.value) {
          InvitationOutcome.DONE -> onDone()
          InvitationOutcome.TOO_SOON -> resend = ResendState.TOO_SOON
          InvitationOutcome.GONE -> onGone()
          InvitationOutcome.NOT_ALLOWED -> viewModel.showFailure(R.string.create_not_allowed)
        }
        is ActionAnswer.Failed -> viewModel.showFailure(answer.text)
      }
      isBusy = false
    }
  }
  Column(Modifier.fillMaxWidth().padding(horizontal = Spacing.l, vertical = Spacing.m)) {
    Row(verticalAlignment = Alignment.CenterVertically) {
      Icon(painterResource(R.drawable.ic_person_add), contentDescription = null, tint = MaterialTheme.colorScheme.onSurfaceVariant)
      Column(Modifier.weight(1f).padding(horizontal = Spacing.m)) {
        Text(invitation.email, maxLines = 1, overflow = TextOverflow.Ellipsis)
        Text(
          invitation.spaceName + " · " + stringResource(R.string.directory_invited_by, invitation.sponsorName),
          style = MaterialTheme.typography.bodySmall,
          color = MaterialTheme.colorScheme.onSurfaceVariant,
          maxLines = 1,
          overflow = TextOverflow.Ellipsis,
        )
        val maybeExpiry = invitation.maybeExpiresAt?.takeIf { !invitation.isExpired }
        if (invitation.isExpired) {
          Text(stringResource(R.string.directory_invitation_expired), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error)
        } else if (maybeExpiry != null) {
          Text(
            stringResource(R.string.directory_invitation_expires, DateFormat.getDateInstance(DateFormat.MEDIUM).format(Date(maybeExpiry))),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
          )
        }
      }
      if (invitation.isGuest) GuestBadge()
    }
    Row(
      Modifier.fillMaxWidth().padding(start = 40.dp),
      horizontalArrangement = Arrangement.spacedBy(Spacing.s),
      verticalAlignment = Alignment.CenterVertically,
    ) {
      when (resend) {
        ResendState.SENT -> StatusText(R.string.directory_resent)
        ResendState.TOO_SOON -> StatusText(R.string.directory_resend_later)
        ResendState.NONE -> if (invitation.canResend) {
          TextButton(onClick = { act({ resendInvitation(invitation.spaceId, invitation.invitationId) }) { resend = ResendState.SENT } }, enabled = !isBusy) {
            Text(stringResource(R.string.directory_resend))
          }
        }
      }
      TextButton(onClick = { isConfirming = true }, enabled = !isBusy) {
        Text(stringResource(R.string.directory_revoke), color = MaterialTheme.colorScheme.error)
      }
    }
  }
  if (isConfirming) {
    AlertDialog(
      onDismissRequest = { isConfirming = false },
      title = { Text(stringResource(R.string.directory_revoke_title)) },
      text = { Text(stringResource(R.string.directory_revoke_text, invitation.email, invitation.spaceName)) },
      confirmButton = {
        TextButton(onClick = {
          isConfirming = false
          act({ revokeInvitation(invitation.spaceId, invitation.invitationId) }, onGone)
        }) { Text(stringResource(R.string.directory_revoke), color = MaterialTheme.colorScheme.error) }
      },
      dismissButton = { TextButton(onClick = { isConfirming = false }) { Text(stringResource(R.string.cancel)) } },
    )
  }
}

@Composable
private fun StatusText(@StringRes text: Int) {
  Text(
    stringResource(text),
    style = MaterialTheme.typography.bodySmall,
    color = MaterialTheme.colorScheme.onSurfaceVariant,
    modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
  )
}

// Shared

/** The search field of a list, and "Invite people" where the user may invite. */
@Composable
private fun ListTools(text: String, onText: (String) -> Unit, @StringRes placeholder: Int, maybeOnInvite: (() -> Unit)?) {
  Row(
    Modifier.fillMaxWidth().padding(horizontal = Spacing.l, vertical = Spacing.s),
    verticalAlignment = Alignment.CenterVertically,
    horizontalArrangement = Arrangement.spacedBy(Spacing.s),
  ) {
    CompactSearchField(value = text, onChange = onText, placeholder = stringResource(placeholder), modifier = Modifier.weight(1f))
    maybeOnInvite?.let { onInvite ->
      OutlinedButton(onClick = onInvite) {
        Icon(painterResource(R.drawable.ic_person_add), contentDescription = null, modifier = Modifier.size(18.dp))
        Text(stringResource(R.string.invite_title), modifier = Modifier.padding(start = Spacing.xs))
      }
    }
  }
}

@Composable
private fun DirectoryHint(@StringRes text: Int) {
  Text(
    stringResource(text),
    style = MaterialTheme.typography.bodyMedium,
    color = MaterialTheme.colorScheme.onSurfaceVariant,
    modifier = Modifier.fillMaxWidth().padding(Spacing.xxl),
  )
}

/** [text] once typing paused, so the server is asked once per pause, not per keystroke. */
@Composable
private fun debounced(text: String): State<String> {
  val settled = remember { mutableStateOf(text.trim()) }
  LaunchedEffect(text) {
    if (text.trim() != settled.value) delay(SEARCH_PAUSE_MS)
    settled.value = text.trim()
  }
  return settled
}

/** One page of a server list: its rows and where the next starts (empty on the last). */
class Page<T>(val rows: List<T>, val next: String)

/**
 * The rows of a server list read a page at a time (#1337): the first page when it starts, the next
 * when the list scrolls to its end. Nothing loads "all of them".
 */
@Stable
class Pager<T>(
  private val scope: CoroutineScope,
  private val idOf: (T) -> String,
  private val fetch: suspend (String) -> PageAnswer<Page<T>>,
) {
  var rows by mutableStateOf(emptyList<T>())
    private set
  var phase by mutableStateOf<ListPhase>(ListPhase.Skeleton)
    private set
  var maybeMoreFailure by mutableStateOf<LoadFailure?>(null)
    private set
  private var next = ""
  private var maybeJob: Job? = null

  val hasMore: Boolean get() = next.isNotEmpty()

  fun reload() {
    maybeJob?.cancel()
    rows = emptyList()
    next = ""
    phase = ListPhase.Skeleton
    maybeMoreFailure = null
    maybeJob = scope.launch { load("") }
  }

  fun loadMore() {
    if (maybeJob?.isActive == true || next.isEmpty()) return
    maybeMoreFailure = null
    val after = next
    maybeJob = scope.launch { load(after) }
  }

  /** Drops rows that are gone on the server (a revoked invitation). */
  fun remove(predicate: (T) -> Boolean) {
    rows = rows.filterNot(predicate)
    if (rows.isEmpty() && next.isEmpty() && phase == ListPhase.Rows) phase = ListPhase.Empty
  }

  private suspend fun load(after: String) {
    when (val answer = fetch(after)) {
      is PageAnswer.Loaded -> {
        // Pages never overlap on the server; a row that moved between two pages shows once.
        val known = rows.mapTo(HashSet(), idOf)
        rows = rows + answer.value.rows.filter { known.add(idOf(it)) }
        next = answer.value.next
        phase = if (rows.isEmpty()) ListPhase.Empty else ListPhase.Rows
      }
      is PageAnswer.Failed -> if (after.isEmpty()) phase = ListPhase.Failed(answer.failure) else maybeMoreFailure = answer.failure
    }
  }
}

/** A [Pager] that starts over whenever one of [keys] changes (a new search, another filter). */
@Composable
fun <T> rememberPager(vararg keys: Any?, idOf: (T) -> String, fetch: suspend (String) -> PageAnswer<Page<T>>): Pager<T> {
  val scope = rememberCoroutineScope()
  val latest by rememberUpdatedState(fetch)
  val pager = remember(*keys) { Pager(scope, idOf) { after -> latest(after) } }
  LaunchedEffect(pager) { pager.reload() }
  return pager
}

/** Below a pager's rows: placeholders or the failure of the first page, else the next page's loader. */
private fun <T> LazyListScope.pagerFooter(pager: Pager<T>, empty: @Composable () -> Unit) {
  placeholderItem(pager.phase, onRetry = pager::reload, empty = empty)
  if (pager.phase != ListPhase.Rows || !pager.hasMore) return
  item(key = "more") {
    val maybeFailure = pager.maybeMoreFailure
    if (maybeFailure != null) {
      Column(Modifier.fillMaxWidth().padding(Spacing.l), horizontalAlignment = Alignment.CenterHorizontally) {
        Text(stringResource(R.string.directory_more_failed), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        TextButton(onClick = pager::loadMore) { Text(stringResource(R.string.retry)) }
      }
    } else {
      // Composed when the list scrolls near its end: that asks for the next page.
      LaunchedEffect(pager.rows.size) { pager.loadMore() }
      Box(Modifier.fillMaxWidth().padding(Spacing.l), contentAlignment = Alignment.Center) {
        CircularProgressIndicator(Modifier.size(24.dp), strokeWidth = 2.dp, color = MaterialTheme.colorScheme.primary.copy(alpha = 0.7f))
      }
    }
  }
}
