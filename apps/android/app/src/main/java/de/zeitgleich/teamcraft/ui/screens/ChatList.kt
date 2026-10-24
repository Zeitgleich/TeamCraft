package de.zeitgleich.teamcraft.ui.screens

import androidx.compose.ui.semantics.heading
import android.content.Context
import android.text.format.DateUtils
import androidx.activity.compose.BackHandler
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Notifications
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.semantics.selected
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Done
import androidx.compose.material.icons.filled.Edit
import androidx.compose.material.icons.filled.Email
import androidx.compose.material.icons.filled.Search
import androidx.compose.material.icons.filled.Star
import androidx.compose.material3.Button
import androidx.compose.material3.ExtendedFloatingActionButton
import androidx.compose.material3.FilterChip
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SwipeToDismissBox
import androidx.compose.material3.SwipeToDismissBoxValue
import androidx.compose.material3.Text
import androidx.compose.material3.rememberSwipeToDismissBoxState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.CustomAccessibilityAction
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.customActions
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.launch
import androidx.compose.foundation.Image
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.automirrored.filled.List
import androidx.compose.material.icons.filled.DateRange
import androidx.compose.material.icons.filled.Home
import androidx.compose.material.icons.filled.Person
import androidx.compose.material3.Badge
import androidx.compose.material3.BadgedBox
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.ListItem
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.Surface
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.collectAsState
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.unit.Dp
import de.zeitgleich.teamcraft.ui.Section
import de.zeitgleich.teamcraft.ui.TabBadge
import de.zeitgleich.teamcraft.ui.activityConversations
import de.zeitgleich.teamcraft.ui.theme.Spacing
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.ChatActions
import de.zeitgleich.teamcraft.ui.ChatListEntry
import de.zeitgleich.teamcraft.ui.ChatPreview
import de.zeitgleich.teamcraft.ui.MainViewModel
import de.zeitgleich.teamcraft.ui.PreviewContent
import de.zeitgleich.teamcraft.ui.PreviewSender
import de.zeitgleich.teamcraft.ui.SectionTitle
import de.zeitgleich.teamcraft.ui.ChatTab
import de.zeitgleich.teamcraft.ui.HomeFilter
import de.zeitgleich.teamcraft.ui.channelSections
import de.zeitgleich.teamcraft.ui.directSections
import de.zeitgleich.teamcraft.ui.isDirect
import de.zeitgleich.teamcraft.ui.tabBadge
import de.zeitgleich.teamcraft.ui.toggled
import de.zeitgleich.teamcraft.ui.selectionActions
import de.zeitgleich.teamcraft.ui.theme.LocalTeamCraftColors
import uniffi.teamcraft_ffi.ConversationItem
import uniffi.teamcraft_ffi.ConversationKind
import uniffi.teamcraft_ffi.Presence

/** Room below the last conversation so the New message button doesn't cover it. */
private val FAB_CLEARANCE = 88.dp
private val AVATAR_SIZE = 40.dp
private val BRAND_LOGO_HEIGHT = 28.dp

/**
 * The main screen, as the desktop's One Home (#237): the brand and the user's avatar on top, one
 * bottom navigation (Home, Activity, Spaces) and one FAB, "New message".
 */
@Composable
fun ChatListScreen(
  viewModel: MainViewModel,
  accountName: String,
  onOpen: (String) -> Unit,
  onSearch: () -> Unit,
  onSettings: () -> Unit,
  onNavigate: (String) -> Unit,
  maybeSelectedId: String? = null,
) {
  var conversations by remember { mutableStateOf<List<ConversationItem>?>(null) }
  var previews by remember { mutableStateOf<Map<String, ChatListEntry>>(emptyMap()) }
  // Long-press starts selecting several conversations, as in WhatsApp; back or ✕ ends it.
  var selection by rememberSaveable { mutableStateOf(listOf<String>()) }
  var maybeNotifying by remember { mutableStateOf<String?>(null) }
  var tab by rememberSaveable { mutableStateOf(ChatTab.Home) }
  BackHandler(enabled = selection.isNotEmpty()) { selection = emptyList() }
  // Back on Activity or Spaces goes Home first, as in other apps with a bottom navigation.
  BackHandler(enabled = selection.isEmpty() && tab != ChatTab.Home) { tab = ChatTab.Home }
  // A conversation opened from elsewhere (notification, link) shows in its own tab.
  LaunchedEffect(maybeSelectedId, conversations) {
    conversations?.firstOrNull { it.id == maybeSelectedId }?.let { selected ->
      if (tab != ChatTab.Activity) tab = if (selected.isDirect) ChatTab.Home else ChatTab.Spaces
    }
  }
  LaunchedEffect(Unit) {
    // Every read finishes before the next starts: while syncing, changes come faster than a full
    // pass over all conversations, and restarting on each would never show the previews.
    viewModel.changeCount.collect {
      val loaded = viewModel.conversations()
      conversations = loaded
      previews = viewModel.chatPreviews(loaded)
    }
  }
  val loaded = conversations
  val maybeBrand by viewModel.loginBrand.collectAsState()
  val isDark = isSystemInDarkTheme()
  val maybeLogo = remember(maybeBrand, isDark) { viewModel.brandLogo(isDark)?.asImageBitmap() }
  val selected = loaded.orEmpty().filter { it.id in selection }
  MainScaffold(
    brand = MainBrand(maybeBrand?.displayName ?: stringResource(R.string.app_name), maybeLogo),
    accountName = accountName,
    tab = tab,
    onTab = { tab = it },
    maybeConversations = loaded,
    list = ListState(
      previews = previews,
      maybeSelectedId = maybeSelectedId,
      selection = selection,
      actionsFor = { conversation, maybeEntry -> rowActions(viewModel, conversation, maybeEntry) },
      onOpen = { id -> if (selection.isEmpty()) onOpen(id) else selection = toggled(selection, id) },
      onLongPress = { id -> selection = toggled(selection, id) },
    ),
    maybeSelectionBar = if (selected.isEmpty()) {
      null
    } else {
      {
        SelectionTopBar(
          viewModel = viewModel,
          selected = selected,
          previews = previews,
          onNotifications = { maybeNotifying = it },
          onDone = { selection = emptyList() },
        )
      }
    },
    onSearch = onSearch,
    onAccount = onSettings,
    onNavigate = onNavigate,
    banners = {
      OfflineBanner(onSignInAgain = { viewModel.signOut() })
      NotificationPermissionCard()
    },
  )
  // Mute or change notifications without opening the conversation.
  maybeNotifying?.let { id -> ConversationNotificationsDialog(viewModel, id) { maybeNotifying = null } }
}

/** The server's brand as the top bar shows it: its name and, when it has one, its logo. */
internal class MainBrand(val name: String, val maybeLogo: ImageBitmap?)

/**
 * The main screen without its data sources, so it can be shown with any data (the screenshots of
 * the androidTest `MainScreenScreenshots`). [maybeSelectionBar] replaces the brand bar while
 * conversations are selected.
 */
@Composable
internal fun MainScaffold(
  brand: MainBrand,
  accountName: String,
  tab: ChatTab,
  onTab: (ChatTab) -> Unit,
  maybeConversations: List<ConversationItem>?,
  list: ListState,
  maybeSelectionBar: (@Composable () -> Unit)?,
  onSearch: () -> Unit,
  onAccount: () -> Unit,
  onNavigate: (String) -> Unit,
  banners: @Composable () -> Unit = {},
) {
  val hasDirect = maybeConversations.orEmpty().any { it.isDirect }
  Scaffold(
    topBar = { maybeSelectionBar?.invoke() ?: BrandTopBar(brand, accountName, onSearch = onSearch, onAccount = onAccount) },
    bottomBar = {
      if (maybeSelectionBar == null) MainNavigation(tab, maybeConversations.orEmpty(), onSelect = onTab)
    },
    floatingActionButton = {
      // Only where it fits and not twice: an empty Home has the same action in its middle.
      if (maybeSelectionBar == null && tab == ChatTab.Home && hasDirect) {
        ExtendedFloatingActionButton(
          onClick = { onNavigate("browse?people=true") },
          icon = { Icon(Icons.Filled.Edit, contentDescription = null) },
          text = { Text(stringResource(R.string.dm_new)) },
        )
      }
    },
  ) { padding ->
    Column(Modifier.fillMaxSize().padding(padding)) {
      banners()
      when {
        maybeConversations == null -> LoadingScreen()
        tab == ChatTab.Home -> HomeTab(maybeConversations, list, onNavigate)
        tab == ChatTab.Activity -> ActivityTab(maybeConversations, list, onNavigate)
        else -> SpacesTab(maybeConversations, list, onNavigate)
      }
    }
  }
}

/** What every tab's conversation rows need. */
internal class ListState(
  val previews: Map<String, ChatListEntry>,
  val maybeSelectedId: String?,
  val selection: List<String>,
  val actionsFor: (ConversationItem, ChatListEntry?) -> RowActions,
  val onOpen: (String) -> Unit,
  val onLongPress: (String) -> Unit,
) {
  val lastActivity: Map<String, Long> get() = previews.mapValues { it.value.preview.sentAt }
}

/** Home: the global entries, then the DMs (outside spaces) with their filters. */
@Composable
private fun HomeTab(conversations: List<ConversationItem>, list: ListState, onNavigate: (String) -> Unit) {
  var dmFilter by rememberSaveable { mutableStateOf("") }
  var homeChip by rememberSaveable { mutableStateOf(HomeFilter.All) }
  if (conversations.none { it.isDirect }) {
    Column(Modifier.fillMaxSize()) {
      HomeEntries(onNavigate)
      RemainingEmptyState(
        icon = Icons.Filled.Email,
        title = stringResource(R.string.dm_empty_title),
        text = stringResource(R.string.dm_empty),
        maybeAction = stringResource(R.string.dm_new) to { onNavigate("browse?people=true") },
      )
    }
    return
  }
  val sections = directSections(conversations, list.lastActivity, dmFilter, homeChip)
  LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(bottom = FAB_CLEARANCE)) {
    item(key = "entries") { HomeEntries(onNavigate) }
    item(key = "filter") { DirectFilter(dmFilter, onChange = { dmFilter = it }) }
    item(key = "chips") { HomeChips(homeChip, onSelect = { homeChip = if (homeChip == it) HomeFilter.All else it }) }
    if (sections.isEmpty()) item(key = "no-match") { EmptyHint(stringResource(R.string.dm_filter_empty)) }
    conversationRows(sections, list)
  }
}

/** Activity: threads and saved items, then every conversation that waits for the user. */
@Composable
private fun ActivityTab(conversations: List<ConversationItem>, list: ListState, onNavigate: (String) -> Unit) {
  val waiting = activityConversations(conversations, list.lastActivity)
  Column(Modifier.fillMaxSize()) {
    EntryRow(Icons.AutoMirrored.Filled.List, stringResource(R.string.threads_title)) { onNavigate("threads") }
    EntryRow(Icons.Filled.Star, stringResource(R.string.saved_title)) { onNavigate("saved") }
    HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
    if (waiting.isEmpty()) {
      RemainingEmptyState(
        icon = Icons.Filled.Done,
        title = stringResource(R.string.activity_empty_title),
        text = stringResource(R.string.activity_empty),
        maybeAction = null,
      )
    } else {
      LazyColumn(Modifier.fillMaxSize()) {
        conversationRows(listOf(Section(SectionTitle.Activity, waiting)), list, hasHeader = false)
      }
    }
  }
}

/** Spaces: the channels by space; browsing for more is the first row (or the empty state's action). */
@Composable
private fun SpacesTab(conversations: List<ConversationItem>, list: ListState, onNavigate: (String) -> Unit) {
  val sections = channelSections(conversations)
  if (sections.isEmpty()) {
    EmptyState(
      icon = Icons.Filled.Search,
      title = stringResource(R.string.spaces_empty_title),
      text = stringResource(R.string.spaces_empty),
      maybeAction = stringResource(R.string.browse_channels) to { onNavigate("browse") },
    )
    return
  }
  LazyColumn(Modifier.fillMaxSize()) {
    item(key = "browse") { EntryRow(Icons.Filled.Add, stringResource(R.string.browse_channels)) { onNavigate("browse") } }
    conversationRows(sections, list)
  }
}

private fun LazyListScope.conversationRows(sections: List<Section>, list: ListState, hasHeader: Boolean = true) {
  for (section in sections) {
    if (hasHeader) item(key = "section-${section.title}") { SectionHeader(section.title) }
    items(section.conversations, key = { "${section.title}-${it.id}" }) { conversation ->
      SwipeableConversationRow(
        actions = list.actionsFor(conversation, list.previews[conversation.id]),
        conversation = conversation,
        maybeEntry = list.previews[conversation.id],
        isSelected = conversation.id == list.maybeSelectedId,
        isChecked = conversation.id in list.selection,
        isSelecting = list.selection.isNotEmpty(),
        onClick = { list.onOpen(conversation.id) },
        onLongPress = { list.onLongPress(conversation.id) },
      )
    }
  }
}

/** The top app bar of the main screen: the server's brand (logo and name) and the user's avatar. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun BrandTopBar(brand: MainBrand, accountName: String, onSearch: () -> Unit, onAccount: () -> Unit) {
  TopAppBar(
    title = {
      Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(Spacing.s)) {
        brand.maybeLogo?.let { logo ->
          // Decorative: the name next to it is what TalkBack reads.
          Image(logo, contentDescription = null, modifier = Modifier.height(BRAND_LOGO_HEIGHT), contentScale = ContentScale.Fit)
        }
        Text(
          brand.name,
          style = MaterialTheme.typography.titleLarge,
          maxLines = 1,
          overflow = TextOverflow.Ellipsis,
          modifier = Modifier.semantics { heading() },
        )
      }
    },
    actions = {
      IconButton(onClick = onSearch) {
        Icon(Icons.Filled.Search, contentDescription = stringResource(R.string.search))
      }
      val description = stringResource(R.string.account_menu, accountName)
      IconButton(onClick = onAccount, modifier = Modifier.semantics { contentDescription = description }) {
        InitialAvatar(accountName, size = 32.dp)
      }
    },
    colors = TopAppBarDefaults.topAppBarColors(containerColor = MaterialTheme.colorScheme.surface),
  )
}

/** Home, Activity and Spaces, each with what is new in it. */
@Composable
private fun MainNavigation(selected: ChatTab, conversations: List<ConversationItem>, onSelect: (ChatTab) -> Unit) {
  val (direct, channels) = conversations.partition { it.isDirect }
  NavigationBar(containerColor = MaterialTheme.colorScheme.surfaceContainer) {
    for (tab in ChatTab.entries) {
      val label = stringResource(
        when (tab) {
          ChatTab.Home -> R.string.nav_home
          ChatTab.Activity -> R.string.nav_activity
          ChatTab.Spaces -> R.string.nav_spaces
        },
      )
      val badge = when (tab) {
        ChatTab.Home -> tabBadge(direct)
        ChatTab.Activity -> TabBadge(mentions = tabBadge(conversations).mentions, unread = 0)
        ChatTab.Spaces -> tabBadge(channels)
      }
      // Channels and Activity count only mentions; DMs count every unread message.
      val count = if (tab == ChatTab.Home) maxOf(badge.mentions, badge.unread) else badge.mentions
      val description = when {
        count > 0 -> pluralStringResource(R.plurals.chat_tab_unread, count, label, count)
        badge.unread > 0 -> stringResource(R.string.chat_tab_new, label)
        else -> label
      }
      NavigationBarItem(
        selected = tab == selected,
        onClick = { onSelect(tab) },
        modifier = Modifier.semantics(mergeDescendants = true) { contentDescription = description },
        label = { Text(label, maxLines = 1, overflow = TextOverflow.Ellipsis) },
        icon = {
          BadgedBox(
            badge = {
              when {
                count > 0 -> Badge(containerColor = LocalTeamCraftColors.current.badge) { Text(count.toString()) }
                badge.unread > 0 -> Badge(containerColor = MaterialTheme.colorScheme.primary)
              }
            },
          ) { NavigationIcon(tab) }
        },
      )
    }
  }
}

@Composable
private fun NavigationIcon(tab: ChatTab) {
  when (tab) {
    ChatTab.Home -> Icon(Icons.Filled.Home, contentDescription = null)
    ChatTab.Activity -> Icon(Icons.Filled.Notifications, contentDescription = null)
    // The channel marker of the lists, so the tab reads as "channels".
    ChatTab.Spaces -> Text("#", style = MaterialTheme.typography.titleLarge, modifier = Modifier.clearAndSetSemantics {})
  }
}

/** The global entries of Home (#237), in one row that never clips: each takes a quarter. */
@Composable
private fun HomeEntries(onNavigate: (String) -> Unit) {
  Row(
    Modifier.fillMaxWidth().padding(horizontal = Spacing.l, vertical = Spacing.s),
    horizontalArrangement = Arrangement.spacedBy(Spacing.s),
  ) {
    for ((route, icon, label) in HOME_ENTRIES) {
      EntryTile(icon, stringResource(label), Modifier.weight(1f)) { onNavigate(route) }
    }
  }
}

@Composable
private fun EntryTile(icon: ImageVector, label: String, modifier: Modifier, onClick: () -> Unit) {
  Surface(
    onClick = onClick,
    shape = MaterialTheme.shapes.medium,
    color = MaterialTheme.colorScheme.surfaceContainer,
    modifier = modifier,
  ) {
    Column(
      Modifier.padding(vertical = Spacing.m, horizontal = Spacing.xs),
      horizontalAlignment = Alignment.CenterHorizontally,
      verticalArrangement = Arrangement.spacedBy(Spacing.xs),
    ) {
      Icon(icon, contentDescription = null, tint = MaterialTheme.colorScheme.primary)
      Text(
        label,
        style = MaterialTheme.typography.labelMedium,
        color = MaterialTheme.colorScheme.onSurface,
        textAlign = TextAlign.Center,
        maxLines = 2,
        overflow = TextOverflow.Ellipsis,
      )
    }
  }
}

/** One entry as a full-width row (Activity, Spaces). */
@Composable
private fun EntryRow(icon: ImageVector, label: String, onClick: () -> Unit) {
  ListItem(
    headlineContent = { Text(label, style = MaterialTheme.typography.titleSmall) },
    leadingContent = { Icon(icon, contentDescription = null, tint = MaterialTheme.colorScheme.primary) },
    modifier = Modifier.clickable(onClick = onClick),
  )
}

/**
 * An empty list: icon, title, text and at most one action, centered in the space that is left
 * (not in the whole screen, which would push it below the middle).
 */
@Composable
private fun ColumnScope.RemainingEmptyState(icon: ImageVector, title: String, text: String, maybeAction: Pair<String, () -> Unit>?) {
  Box(Modifier.fillMaxWidth().weight(1f).padding(horizontal = Spacing.xxl), contentAlignment = Alignment.Center) {
    EmptyStateContent(icon, title, text, maybeAction)
  }
}

@Composable
private fun EmptyState(icon: ImageVector, title: String, text: String, maybeAction: Pair<String, () -> Unit>?) {
  Box(Modifier.fillMaxSize().padding(horizontal = Spacing.xxl), contentAlignment = Alignment.Center) {
    EmptyStateContent(icon, title, text, maybeAction)
  }
}

@Composable
private fun EmptyStateContent(icon: ImageVector, title: String, text: String, maybeAction: Pair<String, () -> Unit>?) {
  Column(
    Modifier.widthIn(max = 360.dp).verticalScroll(rememberScrollState()),
    horizontalAlignment = Alignment.CenterHorizontally,
    verticalArrangement = Arrangement.spacedBy(Spacing.m),
  ) {
    Box(
      Modifier.size(64.dp).background(MaterialTheme.colorScheme.primaryContainer, CircleShape),
      contentAlignment = Alignment.Center,
    ) {
      Icon(icon, contentDescription = null, tint = MaterialTheme.colorScheme.onPrimaryContainer, modifier = Modifier.size(32.dp))
    }
    Text(
      title,
      style = MaterialTheme.typography.titleMedium,
      textAlign = TextAlign.Center,
      modifier = Modifier.semantics { heading() },
    )
    Text(
      text,
      style = MaterialTheme.typography.bodyMedium,
      color = MaterialTheme.colorScheme.onSurfaceVariant,
      textAlign = TextAlign.Center,
    )
    maybeAction?.let { (label, action) ->
      Button(onClick = action, modifier = Modifier.padding(top = Spacing.s)) { Text(label) }
    }
  }
}

/** Unread / People / Groups; pressing the selected chip again shows everything. */
@Composable
private fun HomeChips(selected: HomeFilter, onSelect: (HomeFilter) -> Unit) {
  Row(
    Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()).padding(horizontal = Spacing.l),
    horizontalArrangement = Arrangement.spacedBy(Spacing.s),
  ) {
    for ((chip, label) in listOf(
      HomeFilter.Unread to R.string.home_filter_unread,
      HomeFilter.People to R.string.home_filter_people,
      HomeFilter.Groups to R.string.home_filter_groups,
    )) {
      FilterChip(selected = chip == selected, onClick = { onSelect(chip) }, label = { Text(stringResource(label)) })
    }
  }
}

@Composable
private fun DirectFilter(value: String, onChange: (String) -> Unit) {
  OutlinedTextField(
    value = value,
    onValueChange = onChange,
    modifier = Modifier.fillMaxWidth().padding(horizontal = Spacing.l, vertical = Spacing.xs),
    placeholder = { Text(stringResource(R.string.dm_filter)) },
    leadingIcon = { Icon(Icons.Filled.Search, contentDescription = null) },
    trailingIcon = {
      if (value.isNotEmpty()) {
        IconButton(onClick = { onChange("") }) {
          Icon(Icons.Filled.Close, contentDescription = stringResource(R.string.clear))
        }
      }
    },
    shape = MaterialTheme.shapes.large,
    singleLine = true,
  )
}

@Composable
private fun EmptyHint(text: String) {
  Text(
    text,
    style = MaterialTheme.typography.bodyMedium,
    color = MaterialTheme.colorScheme.onSurfaceVariant,
    textAlign = TextAlign.Center,
    modifier = Modifier.fillMaxWidth().padding(32.dp),
  )
}

@Composable
private fun SectionHeader(title: SectionTitle) {
  val text = when (title) {
    SectionTitle.Favorites -> stringResource(R.string.favorites)
    SectionTitle.Channels -> stringResource(R.string.channels)
    SectionTitle.DirectMessages -> stringResource(R.string.direct_messages)
    SectionTitle.Activity -> stringResource(R.string.nav_activity)
    is SectionTitle.Space -> title.name
  }
  Text(
    text.uppercase(),
    style = MaterialTheme.typography.labelMedium,
    color = MaterialTheme.colorScheme.onSurfaceVariant,
    modifier = Modifier
      .padding(start = 16.dp, top = 16.dp, bottom = 4.dp)
      // Read as written, not spelled out in capitals; TalkBack can jump between sections.
      .semantics {
        heading()
        contentDescription = text
      },
  )
}

/**
 * Swipe right toggles read/unread, swipe left toggles the favorite; both snap back. The same
 * actions are in the long-press menu and in the accessibility actions.
 */
@Composable
private fun SwipeableConversationRow(
  actions: RowActions,
  conversation: ConversationItem,
  maybeEntry: ChatListEntry?,
  isSelected: Boolean,
  isChecked: Boolean,
  isSelecting: Boolean,
  onClick: () -> Unit,
  onLongPress: () -> Unit,
) {
  val haptics = LocalHapticFeedback.current
  val scope = rememberCoroutineScope()
  val state = rememberSwipeToDismissBoxState()
  SwipeToDismissBox(
    state = state,
    gesturesEnabled = !isSelecting,
    enableDismissFromStartToEnd = actions.toggleRead != null,
    backgroundContent = { SwipeBackground(state.dismissDirection, conversation, isUnread = actions.isUnread) },
    onDismiss = { direction ->
      when (direction) {
        SwipeToDismissBoxValue.StartToEnd -> actions.toggleRead?.invoke()
        SwipeToDismissBoxValue.EndToStart -> actions.toggleFavorite()
        SwipeToDismissBoxValue.Settled -> Unit
      }
      haptics.performHapticFeedback(HapticFeedbackType.LongPress)
      // Never removed: the row snaps back.
      scope.launch { state.reset() }
    },
  ) {
    ConversationRow(conversation, maybeEntry, actions, isSelected, isChecked, onClick, onLongPress)
  }
}

/** What can be done with a row, shared by swipe, menu and accessibility. */
internal class RowActions(
  val isUnread: Boolean,
  val toggleRead: (() -> Unit)?,
  val toggleFavorite: () -> Unit,
)

private fun rowActions(viewModel: MainViewModel, conversation: ConversationItem, maybeEntry: ChatListEntry?): RowActions {
  val isUnread = conversation.unreadCount > 0u
  val toggleRead: (() -> Unit)? = when {
    isUnread -> ({ viewModel.markRead(conversation.id) })
    maybeEntry != null -> ({ viewModel.act(ChatActions.markUnread(conversation.id, maybeEntry.lastMessageId)) })
    else -> null
  }
  return RowActions(
    isUnread = isUnread,
    toggleRead = toggleRead,
    toggleFavorite = { viewModel.act(ChatActions.setFavorite(conversation.id, !conversation.isFavorite)) },
  )
}

@Composable
private fun SwipeBackground(direction: SwipeToDismissBoxValue, conversation: ConversationItem, isUnread: Boolean) {
  val (color, icon, label, alignment) = when (direction) {
    SwipeToDismissBoxValue.StartToEnd -> SwipeLook(
      MaterialTheme.colorScheme.primary,
      if (isUnread) Icons.Filled.Done else Icons.Filled.Email,
      if (isUnread) R.string.chat_mark_read else R.string.chat_mark_unread,
      Alignment.CenterStart,
    )
    SwipeToDismissBoxValue.EndToStart -> SwipeLook(
      MaterialTheme.colorScheme.tertiary,
      Icons.Filled.Star,
      if (conversation.isFavorite) R.string.favorite_remove else R.string.favorite_add,
      Alignment.CenterEnd,
    )
    SwipeToDismissBoxValue.Settled -> return
  }
  Box(Modifier.fillMaxSize().background(color).padding(horizontal = 24.dp), contentAlignment = alignment) {
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
      Icon(icon, contentDescription = null, tint = MaterialTheme.colorScheme.onPrimary)
      Text(stringResource(label), color = MaterialTheme.colorScheme.onPrimary, style = MaterialTheme.typography.labelLarge)
    }
  }
}

private data class SwipeLook(
  val color: Color,
  val icon: ImageVector,
  val label: Int,
  val alignment: Alignment,
)

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun ConversationRow(
  conversation: ConversationItem,
  maybeEntry: ChatListEntry?,
  actions: RowActions,
  isSelected: Boolean,
  isChecked: Boolean,
  onClick: () -> Unit,
  onLongPress: () -> Unit,
) {
  val haptics = LocalHapticFeedback.current
  val readLabel = stringResource(if (actions.isUnread) R.string.chat_mark_read else R.string.chat_mark_unread)
  val favoriteLabel = stringResource(if (conversation.isFavorite) R.string.favorite_remove else R.string.favorite_add)
  Box {
    Row(
      modifier = Modifier
        .fillMaxWidth()
        // The open conversation, when it shows next to the list (two panes).
        .background(
          when {
            isChecked -> MaterialTheme.colorScheme.secondaryContainer
            isSelected -> MaterialTheme.colorScheme.primaryContainer
            else -> MaterialTheme.colorScheme.background
          },
        )
        .combinedClickable(
          onClick = onClick,
          onLongClick = {
            haptics.performHapticFeedback(HapticFeedbackType.LongPress)
            onLongPress()
          },
        )
        .semantics { selected = isChecked }
        .semantics {
          customActions = listOfNotNull(
            actions.toggleRead?.let { toggle -> CustomAccessibilityAction(readLabel) { toggle(); true } },
            CustomAccessibilityAction(favoriteLabel) { actions.toggleFavorite(); true },
          )
        }
        .padding(horizontal = 16.dp, vertical = 10.dp),
      verticalAlignment = Alignment.CenterVertically,
    ) {
      if (isChecked) CheckedAvatar() else ConversationAvatar(conversation)
      Column(Modifier.weight(1f).padding(start = 12.dp)) {
        TitleLine(conversation, maybeEntry)
        PreviewLine(conversation, maybeEntry)
      }
    }
  }
}

@Composable
private fun TitleLine(conversation: ConversationItem, maybeEntry: ChatListEntry?) {
  val context = LocalContext.current
  val isUnread = conversation.unreadCount > 0u
  Row(verticalAlignment = Alignment.CenterVertically) {
    Text(
      conversation.name,
      modifier = Modifier.weight(1f),
      maxLines = 1,
      overflow = TextOverflow.Ellipsis,
      style = MaterialTheme.typography.titleSmall,
      fontWeight = if (isUnread) FontWeight.Bold else FontWeight.Normal,
      color = if (conversation.isMuted) MaterialTheme.colorScheme.onSurfaceVariant else MaterialTheme.colorScheme.onSurface,
    )
    if (conversation.isMuted) {
      val mutedLabel = stringResource(R.string.chat_muted)
      Text(
        "🔕",
        style = MaterialTheme.typography.labelSmall,
        modifier = Modifier.padding(start = 4.dp).semantics { contentDescription = mutedLabel },
      )
    }
    maybeEntry?.let { entry ->
      Text(
        listTime(context, entry.preview.sentAt),
        style = MaterialTheme.typography.labelSmall,
        color = if (isUnread) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.padding(start = 8.dp),
      )
    }
  }
}

@Composable
private fun PreviewLine(conversation: ConversationItem, maybeEntry: ChatListEntry?) {
  val isUnread = conversation.unreadCount > 0u
  Row(verticalAlignment = Alignment.CenterVertically) {
    Text(
      maybeEntry?.let { previewText(it.preview) } ?: conversation.topic,
      modifier = Modifier.weight(1f),
      maxLines = 1,
      overflow = TextOverflow.Ellipsis,
      style = MaterialTheme.typography.bodyMedium,
      color = if (isUnread) MaterialTheme.colorScheme.onSurface else MaterialTheme.colorScheme.onSurfaceVariant,
    )
    if (conversation.mentionCount > 0u) {
      val count = conversation.mentionCount.toInt()
      CountPill(
        "@${count}",
        LocalTeamCraftColors.current.badge,
        pluralStringResource(R.plurals.chat_mentions, count, count),
      )
    } else if (isUnread) {
      val count = conversation.unreadCount.toInt()
      CountPill(
        count.toString(),
        if (conversation.isMuted) MaterialTheme.colorScheme.onSurfaceVariant else MaterialTheme.colorScheme.primary,
        pluralStringResource(R.plurals.chat_unread, count, count),
      )
    }
  }
}

@Composable
private fun previewText(preview: ChatPreview): String {
  val body = when (val content = preview.content) {
    is PreviewContent.Text -> content.text
    PreviewContent.Deleted -> stringResource(R.string.chat_preview_deleted)
    PreviewContent.Event -> stringResource(R.string.chat_preview_event)
  }
  return when (val sender = preview.sender) {
    PreviewSender.None -> body
    PreviewSender.Me -> stringResource(R.string.chat_preview_sender, stringResource(R.string.you), body)
    is PreviewSender.Other -> stringResource(R.string.chat_preview_sender, sender.name, body)
  }
}

/** Today: the time; this year: day and month; older: the full date. */
private fun listTime(context: Context, sentAt: Long): String {
  val flags = when {
    DateUtils.isToday(sentAt) -> DateUtils.FORMAT_SHOW_TIME
    System.currentTimeMillis() - sentAt < DateUtils.WEEK_IN_MILLIS -> DateUtils.FORMAT_SHOW_WEEKDAY or DateUtils.FORMAT_ABBREV_WEEKDAY
    else -> DateUtils.FORMAT_SHOW_DATE or DateUtils.FORMAT_ABBREV_MONTH
  }
  return DateUtils.formatDateTime(context, sentAt, flags)
}

/** A round marker with the conversation's initial (people) or its kind (channels). */
@Composable
private fun ConversationAvatar(conversation: ConversationItem) {
  val isPerson = conversation.kind == ConversationKind.DIRECT || conversation.kind == ConversationKind.GROUP
  val label = if (isPerson) conversation.name.firstOrNull()?.uppercase() ?: "?" else prefix(conversation.kind)
  Box {
    Box(
      Modifier
        .size(AVATAR_SIZE)
        .background(
          if (isPerson) avatarColor(conversation.name) else MaterialTheme.colorScheme.surfaceVariant,
          CircleShape,
        ),
      contentAlignment = Alignment.Center,
    ) {
      Text(
        label,
        style = MaterialTheme.typography.titleMedium,
        color = if (isPerson) Color.White else MaterialTheme.colorScheme.onSurfaceVariant,
      )
    }
    conversation.maybePeerPresence?.let { presence ->
      val description = stringResource(presenceLabel(presence))
      Box(
        Modifier
          .align(Alignment.BottomEnd)
          .size(12.dp)
          .background(MaterialTheme.colorScheme.background, CircleShape)
          .padding(2.dp)
          .background(presenceColor(presence), CircleShape)
          .semantics { contentDescription = description },
      )
    }
  }
}

private fun presenceColor(presence: Presence): Color = when (presence) {
  Presence.ONLINE -> Color(0xFF2E9E5B)
  Presence.AWAY -> Color(0xFFE0A030)
  Presence.DO_NOT_DISTURB -> Color(0xFFD0453A)
  Presence.OFFLINE -> Color(0xFF8A8F98)
}

private fun presenceLabel(presence: Presence): Int = when (presence) {
  Presence.ONLINE -> R.string.presence_online
  Presence.AWAY -> R.string.presence_away
  Presence.DO_NOT_DISTURB -> R.string.presence_dnd
  Presence.OFFLINE -> R.string.presence_peer_offline
}

/** A stable hue per name, so people are recognizable at a glance. */
private fun avatarColor(name: String): Color = Color.hsl(((name.hashCode() % 360 + 360) % 360).toFloat(), 0.45f, 0.45f)

@Composable
private fun CountPill(text: String, color: Color, description: String) {
  Text(
    text,
    color = LocalTeamCraftColors.current.chromeText,
    style = MaterialTheme.typography.labelSmall,
    modifier = Modifier
      .padding(start = 8.dp)
      .background(color, RoundedCornerShape(10.dp))
      .padding(horizontal = 7.dp, vertical = 2.dp)
      .semantics { contentDescription = description },
  )
}

/** The sidebar's markers: `#` public, lock-like `◆` private, `@` direct. */
private fun prefix(kind: ConversationKind): String = when (kind) {
  ConversationKind.PUBLIC -> "#"
  ConversationKind.PRIVATE -> "◆"
  ConversationKind.DIRECT -> "@"
  ConversationKind.GROUP -> "@"
  ConversationKind.INTEGRATION -> "⚙"
}

/** The global entries of Home: route, icon, label. */
private val HOME_ENTRIES = listOf(
  Triple("threads", Icons.AutoMirrored.Filled.List, R.string.home_entry_threads),
  Triple("saved", Icons.Filled.Star, R.string.home_entry_saved),
  Triple("meetings", Icons.Filled.DateRange, R.string.home_entry_meetings),
  Triple("browse?people=true", Icons.Filled.Person, R.string.home_entry_people),
)

/** Instead of the title while conversations are selected: how many, and what to do with them. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun SelectionTopBar(
  viewModel: MainViewModel,
  selected: List<ConversationItem>,
  previews: Map<String, ChatListEntry>,
  onNotifications: (String) -> Unit,
  onDone: () -> Unit,
) {
  val actions = selectionActions(selected)
  TopAppBar(
    title = { Text(pluralStringResource(R.plurals.chats_selected, selected.size, selected.size)) },
    navigationIcon = {
      IconButton(onClick = onDone) { Icon(Icons.Filled.Close, contentDescription = stringResource(R.string.cancel)) }
    },
    actions = {
      TextButton(onClick = {
        for (conversation in selected) {
          if (actions.isMarkingRead) {
            viewModel.markRead(conversation.id)
          } else {
            previews[conversation.id]?.let { viewModel.act(ChatActions.markUnread(conversation.id, it.lastMessageId)) }
          }
        }
        onDone()
      }) { Text(stringResource(if (actions.isMarkingRead) R.string.chat_mark_read else R.string.chat_mark_unread)) }
      IconButton(onClick = {
        for (conversation in selected) viewModel.act(ChatActions.setFavorite(conversation.id, actions.isAddingFavorite))
        onDone()
      }) {
        Icon(
          Icons.Filled.Star,
          contentDescription = stringResource(if (actions.isAddingFavorite) R.string.favorite_add else R.string.favorite_remove),
        )
      }
      actions.maybeSingle?.let { id ->
        IconButton(onClick = {
          onNotifications(id)
          onDone()
        }) { Icon(Icons.Filled.Notifications, contentDescription = stringResource(R.string.notifications_conversation)) }
      }
    },
  )
}

/** The avatar of a selected conversation: a check mark. */
@Composable
private fun CheckedAvatar() {
  Box(Modifier.size(AVATAR_SIZE).background(MaterialTheme.colorScheme.primary, CircleShape), contentAlignment = Alignment.Center) {
    Icon(Icons.Filled.Done, contentDescription = null, tint = MaterialTheme.colorScheme.onPrimary)
  }
}

/** The user's own avatar: the initial on the stable hue of the name, as for other people. */
@Composable
private fun InitialAvatar(name: String, size: Dp) {
  Box(Modifier.size(size).background(avatarColor(name), CircleShape), contentAlignment = Alignment.Center) {
    Text(
      name.firstOrNull()?.uppercase() ?: "?",
      style = MaterialTheme.typography.titleSmall,
      color = Color.White,
      modifier = Modifier.clearAndSetSemantics {},
    )
  }
}
