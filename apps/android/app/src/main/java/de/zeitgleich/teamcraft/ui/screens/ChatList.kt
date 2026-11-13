package de.zeitgleich.teamcraft.ui.screens

import de.zeitgleich.teamcraft.ui.PresenceDot
import de.zeitgleich.teamcraft.ui.presenceLabel
import androidx.compose.ui.semantics.heading
import androidx.compose.animation.core.Spring
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.spring
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.width
import androidx.compose.material.icons.filled.KeyboardArrowDown
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.semantics.stateDescription
import android.content.Context
import android.text.format.DateUtils
import androidx.activity.compose.BackHandler
import androidx.compose.material.icons.filled.Call
import androidx.activity.compose.ReportDrawnWhen
import androidx.compose.material.icons.filled.Lock
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.lazy.LazyListState
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
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.WindowInsetsSides
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.only
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawing
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
import androidx.compose.material3.FilterChip
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SwipeToDismissBox
import androidx.compose.material3.SwipeToDismissBoxValue
import androidx.compose.material3.Text
import androidx.compose.material3.rememberSwipeToDismissBoxState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
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
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.ProcessLifecycleOwner
import androidx.lifecycle.repeatOnLifecycle
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.CustomAccessibilityAction
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.customActions
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.ui.theme.FavoritesSort
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
import androidx.compose.material.icons.automirrored.filled.KeyboardArrowRight
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material.icons.automirrored.filled.List
import androidx.compose.material.icons.filled.DateRange
import androidx.compose.material.icons.filled.Home
import androidx.compose.material.icons.filled.Person
import androidx.compose.material3.Badge
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.Surface
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.collectAsState
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.unit.Dp
import de.zeitgleich.teamcraft.ui.HomeSpace
import de.zeitgleich.teamcraft.ui.SelectedSpace
import de.zeitgleich.teamcraft.ui.hasNewElsewhere
import de.zeitgleich.teamcraft.ui.homeSpaces
import de.zeitgleich.teamcraft.ui.selectedSpace
import de.zeitgleich.teamcraft.ui.spaceHomeSections
import de.zeitgleich.teamcraft.ui.spaceId
import de.zeitgleich.teamcraft.ui.withEmptySpaces
import de.zeitgleich.teamcraft.ui.TypingDots
import de.zeitgleich.teamcraft.ui.typingLine
import de.zeitgleich.teamcraft.ui.typingText
import de.zeitgleich.teamcraft.ui.SpacePicture
import de.zeitgleich.teamcraft.ui.Section
import de.zeitgleich.teamcraft.ui.TabBadge
import de.zeitgleich.teamcraft.ui.activityConversations
import de.zeitgleich.teamcraft.ui.theme.Spacing
import de.zeitgleich.teamcraft.ui.theme.darkStatusBarBackdrop
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.luminance
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.ChatActions
import de.zeitgleich.teamcraft.core.ChatQueries
import de.zeitgleich.teamcraft.core.ExpiringGuest
import de.zeitgleich.teamcraft.core.LoadFailure
import de.zeitgleich.teamcraft.ui.ListPhase
import de.zeitgleich.teamcraft.ui.ChatListEntry
import de.zeitgleich.teamcraft.ui.ChatPreview
import de.zeitgleich.teamcraft.ui.MainViewModel
import de.zeitgleich.teamcraft.ui.PreviewContent
import de.zeitgleich.teamcraft.ui.PreviewSender
import de.zeitgleich.teamcraft.ui.SectionTitle
import de.zeitgleich.teamcraft.ui.ChatTab
import de.zeitgleich.teamcraft.ui.HomeFilter
import de.zeitgleich.teamcraft.ui.directSections
import de.zeitgleich.teamcraft.ui.isDirect
import de.zeitgleich.teamcraft.ui.listSections
import de.zeitgleich.teamcraft.ui.theme.AppearancePrefs
import de.zeitgleich.teamcraft.ui.theme.ChatListGrouping
import de.zeitgleich.teamcraft.ui.theme.ChatListLayout
import de.zeitgleich.teamcraft.ui.CollapsedSections
import de.zeitgleich.teamcraft.ui.collapseKey
import de.zeitgleich.teamcraft.ui.homeSections
import de.zeitgleich.teamcraft.ui.visibleRows
import de.zeitgleich.teamcraft.core.ScheduledOverview
import de.zeitgleich.teamcraft.ui.listName
import de.zeitgleich.teamcraft.ui.tabBadge
import de.zeitgleich.teamcraft.ui.toggled
import de.zeitgleich.teamcraft.ui.selectionActions
import de.zeitgleich.teamcraft.ui.UserAvatar
import de.zeitgleich.teamcraft.ui.avatarColor
import de.zeitgleich.teamcraft.ui.conversationAvatarColor
import de.zeitgleich.teamcraft.ui.isWithBot
import de.zeitgleich.teamcraft.ui.theme.LocalTeamCraftColors
import uniffi.teamcraft_ffi.FirstSyncItem
import uniffi.teamcraft_ffi.ConversationItem
import uniffi.teamcraft_ffi.JoinRequestItem
import uniffi.teamcraft_ffi.ConversationKind
import uniffi.teamcraft_ffi.Presence
import uniffi.teamcraft_ffi.RowBadge
import uniffi.teamcraft_ffi.RowBadgeTone
import uniffi.teamcraft_ffi.rowBadge
import androidx.compose.material3.FloatingToolbarDefaults
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.layout.navigationBars
import androidx.compose.material3.ExtendedFloatingActionButton
import androidx.compose.runtime.derivedStateOf

private val AVATAR_SIZE = 40.dp
private val BRAND_LOGO_HEIGHT = 28.dp
private val QUICK_CARD_WIDTH = 148.dp

/**
 * The main screen, as the desktop's One Home (#237): the brand and the user's avatar on top, one
 * bottom navigation (Home, Activity, Spaces) and one FAB, "New message".
 */
@Composable
fun ChatListScreen(
  viewModel: MainViewModel,
  accountUserId: String,
  accountName: String,
  onOpen: (String) -> Unit,
  onSearch: () -> Unit,
  onSettings: () -> Unit,
  onNavigate: (String) -> Unit,
  maybeSelectedId: String? = null,
) {
  val kept by viewModel.chatList.collectAsState()
  val typing by viewModel.typingNames.collectAsState()
  val scope = rememberCoroutineScope()
  val conversations = kept.maybeConversations
  val previews = kept.previews
  // Admins' requests to join channels the user owns: on top of Activity and on its badge.
  var joinRequests by remember { mutableStateOf<List<JoinRequestItem>>(emptyList()) }
  var expiringGuests by remember { mutableStateOf<List<ExpiringGuest>>(emptyList()) }
  // Long-press starts selecting several conversations, as in WhatsApp; back or the close button ends it.
  var selection by rememberSaveable { mutableStateOf(listOf<String>()) }
  var maybeNotifying by remember { mutableStateOf<String?>(null) }
  var tab by rememberSaveable { mutableStateOf(ChatTab.Home) }
  // An account stays a guest or not; read again only when the list changes size (the first sync
  // is what names the user).
  var isGuest by remember { mutableStateOf(false) }
  // The spaces the user belongs to (#1526), also those without a joined channel: from the same
  // snapshot read, again when the list's spaces change or the switcher opens, never per change.
  var snapshotSpaces by remember { mutableStateOf<List<HomeSpace>>(emptyList()) }
  var isSwitchingSpace by rememberSaveable { mutableStateOf(false) }
  // The "New" menu and its forms (#1534); a space made here is read again at once (it has no channel).
  var maybeCreating by rememberSaveable { mutableStateOf<CreateStep?>(null) }
  var maybeNewChannelSpaceId by rememberSaveable { mutableStateOf<String?>(null) }
  var createdSpaces by rememberSaveable { mutableIntStateOf(0) }
  // Spaces where the user may create channels, and whether new spaces: asked when the menu or the
  // switcher opens, so a role changed meanwhile counts.
  var channelSpaceIds by remember { mutableStateOf<Set<String>>(emptySet()) }
  var canCreateSpaces by remember { mutableStateOf(false) }
  val isChoosing = maybeCreating == CreateStep.Menu || isSwitchingSpace
  val listedSpaceIds = remember(conversations) { conversations?.mapNotNullTo(HashSet()) { it.maybeSpaceId } }
  LaunchedEffect(conversations?.size, listedSpaceIds, isSwitchingSpace, createdSpaces) {
    val maybeSnapshot = viewModel.readChat { snapshot() } ?: return@LaunchedEffect
    isGuest = maybeSnapshot.me.isGuest
    snapshotSpaces = maybeSnapshot.spaces.map { HomeSpace(it.id, it.name, it.maybeImageVersion) }
    channelSpaceIds = maybeSnapshot.spaces.filter { it.canCreateChannels }.mapTo(HashSet()) { it.id }
  }
  LaunchedEffect(isChoosing, isGuest) {
    if (isChoosing && !isGuest) canCreateSpaces = viewModel.canCreateSpaces()
  }
  val spaces = remember(snapshotSpaces, conversations) { homeSpaces(snapshotSpaces, conversations.orEmpty()) }
  // Takes effect at once: the flow follows the setting (#1151).
  val context = LocalContext.current
  val spaceChoice = remember { SelectedSpace.of(context) }
  val maybeRememberedSpace by spaceChoice.maybeId.collectAsState()
  val layout by remember { AppearancePrefs.prefs(context).chatListLayout }.collectAsState()
  val grouping by remember { AppearancePrefs.prefs(context).chatListGrouping }.collectAsState()
  val isList = layout == ChatListLayout.List
  BackHandler(enabled = selection.isNotEmpty()) { selection = emptyList() }
  // Back on DMs or Activity goes Home first, as in other apps with a bottom navigation.
  BackHandler(enabled = selection.isEmpty() && tab != ChatTab.Home) { tab = ChatTab.Home }
  // A channel opened from elsewhere (notification, link) while the DMs show: Home has it. Home
  // lists DMs with something new too, so a DM opened from there stays highlighted there.
  LaunchedEffect(maybeSelectedId, conversations, isList) {
    conversations?.firstOrNull { it.id == maybeSelectedId }?.let { selected ->
      if (tab == ChatTab.Direct && (isList || !selected.isDirect)) tab = ChatTab.Home
    }
  }
  var maybeFirstSync by remember { mutableStateOf<FirstSyncItem?>(null) }
  LaunchedEffect(Unit) {
    // Only while the app shows (issue #1819): in the background, a catch-up's changes each read the
    // whole chat list for nobody, and the reads the sync waits for queued behind them. Back in
    // front, the counter's current value reads the list once.
    ProcessLifecycleOwner.get().lifecycle.repeatOnLifecycle(Lifecycle.State.STARTED) {
      // Every read finishes before the next starts: while syncing, changes come faster than a full
      // pass over all conversations, and restarting on each would never show the previews.
      viewModel.changeCount.collect {
        viewModel.refreshChatList(accountUserId)
        maybeFirstSync = viewModel.firstSync()
        joinRequests = viewModel.joinRequests.list()
        expiringGuests = viewModel.readChat { query(ChatQueries.expiringGuests()).guests } ?: emptyList()
      }
    }
  }
  val loaded = conversations
  // The read the attention banner made already: its counts feed the Scheduled card too (#1466).
  val maybeScheduled = rememberScheduledOverview(viewModel)
  // The list is usable once its rows and their last lines are there (start-up measurements, Play vitals).
  ReportDrawnWhen { conversations != null && (previews.isNotEmpty() || conversations.isEmpty()) }
  val maybeBrand by viewModel.loginBrand.collectAsState()
  val isDark = isSystemInDarkTheme()
  val maybeLogo = remember(maybeBrand, isDark) { viewModel.brandLogo(isDark)?.asImageBitmap() }
  val selected = loaded.orEmpty().filter { it.id in selection }
  // A decision on a request to join that didn't go through, among others.
  val snackbars = rememberFailureSnackbars(viewModel)
  val suspectDeletionsText = stringResource(R.string.deletions_suspect)
  // The server claimed many conversations deleted at once (e2ee.md §4.6): told once.
  LaunchedEffect(conversations?.size) {
    val count = viewModel.takeSuspectDeletions()
    if (count > 0u) snackbars.showSnackbar(suspectDeletionsText.format(count.toInt()))
  }
  MainScaffold(
    brand = MainBrand(maybeBrand?.displayName ?: stringResource(R.string.app_name), maybeLogo),
    accountUserId = accountUserId,
    accountName = accountName,
    layout = layout,
    grouping = grouping,
    spaces = spaces,
    maybeRememberedSpaceId = maybeRememberedSpace,
    onSwitchSpace = { isSwitchingSpace = true },
    tab = tab,
    onTab = { tab = it },
    maybeConversations = loaded,
    isSyncing = maybeFirstSync != null,
    maybeReadFailure = kept.maybeReadFailure,
    onRetry = {
      viewModel.retryChatList()
      scope.launch { viewModel.refreshChatList(accountUserId) }
    },
    list = ListState(
      previews = previews,
      runningCalls = kept.runningCalls,
      typing = typing,
      maybeSelectedId = maybeSelectedId,
      selection = selection,
      actionsFor = { conversation, maybeEntry -> rowActions(viewModel, conversation, maybeEntry) },
      onOpen = { id -> if (selection.isEmpty()) onOpen(id) else selection = toggled(selection, id) },
      onLongPress = { id -> selection = toggled(selection, id) },
      conversationMenu = { conversation, isOpen, onDismiss ->
        ConversationEntryMenu(
          viewModel = viewModel,
          conversation = conversation,
          isOpen = isOpen,
          onDismiss = onDismiss,
          onSelect = { selection = toggled(selection, conversation.id) },
          onNotifications = { maybeNotifying = conversation.id },
        )
      },
      spaceMenu = { spaceId, name, isOpen, onDismiss ->
        SpaceEntryMenu(
          viewModel,
          spaceId,
          name,
          isOpen,
          onDismiss,
          maybeOnOpenView = { view -> onNavigate("space/$spaceId?tab=${view.name}") },
          maybeOnBrowse = if (isGuest) null else ({ onNavigate("browse?space=$spaceId") }),
          maybeOnNewChannel = if (spaceId in channelSpaceIds) {
            {
              maybeNewChannelSpaceId = spaceId
              maybeCreating = CreateStep.Channel
            }
          } else {
            null
          },
        )
      },
      onBrowseSpace = { spaceId -> onNavigate("browse?space=$spaceId") },
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
    onCreate = { maybeCreating = CreateStep.Menu },
    banners = {
      OfflineBanner(onSignInAgain = { viewModel.signOut() })
      val firstSync = maybeFirstSync
      // A first sync has its own bar.
      if (firstSync != null) {
        FirstSyncBar(firstSync)
      } else {
        SyncingLine(isCatchingUp = conversations.orEmpty().any { it.isCatchingUp })
      }
      NotificationPermissionCard()
      ScheduledAttentionCard(maybeScheduled) { onNavigate("scheduled") }
    },
    isGuest = isGuest,
    quickAccess = quickAccess(conversations.orEmpty(), maybeScheduled),
    joinRequestCount = joinRequests.size,
    joinRequests = { JoinRequestSection(joinRequests, viewModel.joinRequests) },
    guestNoticeCount = expiringGuests.size,
    guestNotices = {
      GuestNoticeSection(expiringGuests) { guest, expiresAtMs ->
        viewModel.act(ChatActions.extendGuest(guest.spaceId, guest.user.id, expiresAtMs))
      }
    },
    snackbarHost = { SnackbarHost(snackbars) },
  )
  // Mute or change notifications without opening the conversation.
  maybeNotifying?.let { id -> ConversationNotificationsDialog(viewModel, id) { maybeNotifying = null } }
  if (isSwitchingSpace) {
    SpaceSwitcherSheet(
      spaces = spaces,
      maybeSelectedId = selectedSpace(spaces, maybeRememberedSpace)?.id,
      canBrowse = !isGuest,
      canCreate = canCreateSpaces,
      onCreate = {
        isSwitchingSpace = false
        maybeCreating = CreateStep.Space
      },
      onSelect = { spaceId ->
        spaceChoice.select(spaceId)
        tab = ChatTab.Home
        isSwitchingSpace = false
      },
      onBrowse = {
        isSwitchingSpace = false
        onNavigate("spaces/browse")
      },
      onDismiss = { isSwitchingSpace = false },
    )
  }
  // A new channel goes into the space Home shows; in the list layout into the only space where the
  // user may create one, else each space's menu has "New channel".
  val maybeChannelSpace = (if (isList) null else selectedSpace(spaces, maybeRememberedSpace)?.takeIf { it.id in channelSpaceIds })
    ?: spaces.singleOrNull { it.id in channelSpaceIds }.takeIf { isList }
  when (maybeCreating) {
    CreateStep.Menu -> CreateMenuSheet(
      choices = CreateChoices(maybeChannelSpace?.name, canCreateSpace = canCreateSpaces, canBrowse = !isGuest),
      onNewMessage = {
        maybeCreating = null
        onNavigate("browse?people=true")
      },
      onNewChannel = {
        maybeNewChannelSpaceId = maybeChannelSpace?.id
        maybeCreating = CreateStep.Channel
      },
      onNewSpace = { maybeCreating = CreateStep.Space },
      onBrowse = {
        maybeCreating = null
        onNavigate(maybeChannelSpace?.let { "browse?space=${it.id}" } ?: "browse")
      },
      onDirectory = {
        maybeCreating = null
        onNavigate("directory")
      },
      onDismiss = { maybeCreating = null },
    )
    CreateStep.Channel -> {
      val maybeSpace = spaces.firstOrNull { it.id == maybeNewChannelSpaceId }
      if (maybeSpace == null) {
        // The space is gone (left, or the list changed): nothing to create a channel in.
        LaunchedEffect(Unit) { maybeCreating = null }
      } else {
        NewChannelSheet(
          viewModel = viewModel,
          spaceId = maybeSpace.id,
          spaceName = maybeSpace.name,
          onCreated = { id ->
            maybeCreating = null
            onOpen(id)
          },
          onDismiss = { maybeCreating = null },
        )
      }
    }
    CreateStep.Space -> NewSpaceSheet(
      viewModel = viewModel,
      onCreated = { id ->
        maybeCreating = null
        createdSpaces++
        spaceChoice.select(id)
        tab = ChatTab.Home
      },
      onDismiss = { maybeCreating = null },
    )
    null -> Unit
  }
}

/** Which part of "New" is open (#1534). */
internal enum class CreateStep { Menu, Channel, Space }

/** The server's brand as the top bar shows it: its name and, when it has one, its logo. */
internal class MainBrand(val name: String, val maybeLogo: ImageBitmap?)

/** Home's header in the spaces layout (#1526): the selected space instead of the brand. */
internal class SpaceHeader(val space: HomeSpace, val hasNewElsewhere: Boolean, val onSwitch: () -> Unit)

/**
 * The main screen without its data sources, so it can be shown with any data (the screenshots of
 * the androidTest `MainScreenScreenshots`). [maybeSelectionBar] replaces the brand bar while
 * conversations are selected.
 */
@Composable
internal fun MainScaffold(
  brand: MainBrand,
  accountUserId: String,
  accountName: String,
  layout: ChatListLayout = ChatListLayout.Spaces,
  grouping: ChatListGrouping = ChatListGrouping.Mixed,
  /** The spaces the user belongs to, by name, with what is new in each (#1526). */
  spaces: List<HomeSpace> = emptyList(),
  /** The space Home of the spaces layout showed last on this device. */
  maybeRememberedSpaceId: String? = null,
  /** Opens the space switcher (the header's space name). */
  onSwitchSpace: () -> Unit = {},
  tab: ChatTab,
  onTab: (ChatTab) -> Unit,
  maybeConversations: List<ConversationItem>?,
  list: ListState,
  maybeSelectionBar: (@Composable () -> Unit)?,
  onSearch: () -> Unit,
  onAccount: () -> Unit,
  onNavigate: (String) -> Unit,
  /** Home's button: the "New" menu (#1534). */
  onCreate: () -> Unit = {},
  banners: @Composable () -> Unit = {},
  /** The first sync is running: an empty list is not known to be empty yet (#297). */
  isSyncing: Boolean = false,
  /** Set when there are no conversations and reading them failed. */
  maybeReadFailure: LoadFailure? = null,
  onRetry: () -> Unit = {},
  /** Guests browse no channels (as on desktop); Home lists only their own. */
  isGuest: Boolean = false,
  /** What the quick-access cards on top of Home say (#1488). */
  quickAccess: QuickAccess = QuickAccess(),
  /** Admins' requests waiting for the user's decision: counted on Activity, shown on top of it. */
  joinRequestCount: Int = 0,
  joinRequests: @Composable () -> Unit = {},
  guestNoticeCount: Int = 0,
  guestNotices: @Composable () -> Unit = {},
  snackbarHost: @Composable () -> Unit = {},
) {
  val isList = layout == ChatListLayout.List
  // The list layout has the DMs on Home and no tab of their own for them.
  val shownTab = if (isList && tab == ChatTab.Direct) ChatTab.Home else tab
  // The spaces layout shows one space at a time on Home and names it in the header (#1526).
  val maybeSpace = if (isList) null else selectedSpace(spaces, maybeRememberedSpaceId)
  val hasDirect = maybeConversations.orEmpty().any { it.isDirect }
  // The button shrinks to its icon once the list scrolls, so it never covers a row's text.
  val homeList = rememberLazyListState()
  val directList = rememberLazyListState()
  val isFabExpanded by remember(shownTab) {
    derivedStateOf { (if (shownTab == ChatTab.Direct) directList else homeList).firstVisibleItemIndex == 0 }
  }
  // The theme's colour (#202) fills the header and the status bar behind it; the lists sit on a
  // neutral card with rounded top corners, so the theme never colours content.
  Scaffold(
    // Edge to edge: the lists' card reaches under the navigation bar; the floating tab bar and the
    // lists' bottom room keep content clear of it.
    contentWindowInsets = ScreenInsets.only(WindowInsetsSides.Top + WindowInsetsSides.Horizontal),
    containerColor = LocalTeamCraftColors.current.chrome,
    topBar = {
      maybeSelectionBar?.invoke() ?: BrandTopBar(
        brand,
        accountUserId,
        accountName,
        onSearch = onSearch,
        onAccount = onAccount,
        maybeSpaceHeader = maybeSpace?.takeIf { shownTab == ChatTab.Home }?.let { space ->
          SpaceHeader(space, hasNewElsewhere(spaces, space.id), onSwitchSpace)
        },
        spaceMenu = list.spaceMenu,
      )
    },
    snackbarHost = snackbarHost,
  ) { padding ->
    Box(Modifier.fillMaxSize().padding(padding)) {
      Column(
        Modifier
          .fillMaxSize()
          .clip(RoundedCornerShape(topStart = CONTENT_CARD_RADIUS, topEnd = CONTENT_CARD_RADIUS))
          .background(MaterialTheme.colorScheme.background),
      ) {
        banners()
        when {
          maybeConversations == null && maybeReadFailure != null ->
            ListFailed(ListPhase.Failed(maybeReadFailure), onRetry)
          maybeConversations == null -> LoadingScreen()
          // Nothing yet while the first sync brings the conversations: not "no conversations".
          maybeConversations.isEmpty() && isSyncing && shownTab != ChatTab.Activity -> ListSkeleton()
          isList && shownTab == ChatTab.Home && grouping == ChatListGrouping.BySpace ->
            ListBySpaceTab(maybeConversations, spaces, list, homeList, onNavigate, canBrowse = !isGuest, quickAccess)
          isList && shownTab == ChatTab.Home ->
            ListTab(maybeConversations, spaces, list, homeList, onNavigate, canBrowse = !isGuest, quickAccess)
          shownTab == ChatTab.Home -> HomeTab(maybeConversations, maybeSpace, list, homeList, onNavigate, canBrowse = !isGuest, quickAccess)
          shownTab == ChatTab.Direct -> DirectTab(maybeConversations, list, directList, onNavigate)
          else -> ActivityTab(maybeConversations, list, onNavigate, joinRequestCount, joinRequests, guestNotices = guestNotices, guestNoticeCount = guestNoticeCount, isSyncing = isSyncing)
        }
      }
      if (maybeSelectionBar == null) {
        val barModifier = Modifier.windowInsetsPadding(WindowInsets.navigationBars).padding(bottom = FloatingToolbarDefaults.ScreenOffset)
        FloatingTabBar(
          selected = shownTab,
          isList = isList,
          conversations = maybeConversations.orEmpty(),
          joinRequestCount = joinRequestCount,
          onSelect = onTab,
          modifier = Modifier.align(Alignment.BottomCenter).then(barModifier),
        )
        // Home and DMs, not twice: empty DMs have the same action in the middle. On Home it opens
        // the "New" menu (message, channel, space); on DMs it starts a message. It sits above the
        // tab bar at the end, as the floating action button of Google's apps.
        if (shownTab == ChatTab.Home || (shownTab == ChatTab.Direct && hasDirect)) {
          val isHome = shownTab == ChatTab.Home
          ExtendedFloatingActionButton(
            onClick = if (isHome) onCreate else ({ onNavigate("browse?people=true") }),
            expanded = isFabExpanded,
            icon = { Icon(if (isHome) Icons.Filled.Add else Icons.Filled.Edit, contentDescription = null) },
            text = { Text(stringResource(if (isHome) R.string.create_new else R.string.dm_new)) },
            modifier = Modifier
              .align(Alignment.BottomEnd)
              .then(barModifier)
              .padding(end = Spacing.l, bottom = FloatingToolbarDefaults.ContainerSize + Spacing.l),
          )
        }
      }
    }
  }
}

/** What every tab's conversation rows need. */
internal class ListState(
  val previews: Map<String, ChatListEntry>,
  /** Conversations with a call running and how many are in it: their rows say so (issue #1352). */
  val runningCalls: Map<String, Int> = emptyMap(),
  /** Who types in each conversation right now (#882): its row says so instead of the preview. */
  val typing: Map<String, List<String>> = emptyMap(),
  val maybeSelectedId: String?,
  val selection: List<String>,
  val actionsFor: (ConversationItem, ChatListEntry?) -> RowActions,
  val onOpen: (String) -> Unit,
  val onLongPress: (String) -> Unit,
  /** The long-press menu of a conversation: the conversation, whether it is open, how to close it. */
  val conversationMenu: @Composable (ConversationItem, Boolean, () -> Unit) -> Unit = { _, _, _ -> },
  /** The long-press menu of a space header: id, name, whether it is open, how to close it. */
  val spaceMenu: @Composable (String, String, Boolean, () -> Unit) -> Unit = { _, _, _, _ -> },
  /** Browses the channels of a space, that space first (#1526). */
  val onBrowseSpace: (String) -> Unit = {},
) {
  val lastActivity: Map<String, Long> get() = previews.mapValues { it.value.preview.sentAt }
}

/**
 * Home of the spaces layout (#1488): the quick-access cards, the DMs with something new, the
 * favorites and each space's channels in sections the user can collapse, and browsing for more at
 * the end. One `LazyColumn`, so only what is on screen is built.
 */
@Composable
private fun HomeTab(
  conversations: List<ConversationItem>,
  maybeSpace: HomeSpace?,
  list: ListState,
  listState: LazyListState,
  onNavigate: (String) -> Unit,
  canBrowse: Boolean,
  quickAccess: QuickAccess,
) {
  // Without spaces (yet), Home lists what there is.
  val context = LocalContext.current
  val favoritesSort by remember { AppearancePrefs.prefs(context).favoritesSortChoice }.collectAsState()
  val sections = maybeSpace
    ?.let { spaceHomeSections(conversations, it.id, list.lastActivity, listsArchived = !canBrowse, favoritesSort = favoritesSort) }
    ?: homeSections(conversations, list.lastActivity, listsArchived = !canBrowse, favoritesSort = favoritesSort)
  val collapsing = remember { CollapsedSections.of(context) }
  val collapsed by collapsing.collapsed.collectAsState()
  LazyColumn(Modifier.fillMaxSize(), state = listState, contentPadding = PaddingValues(bottom = floatingTabBarClearance())) {
    item(key = "quick-access") { QuickAccessRow(quickAccess, onNavigate) }
    if (sections.isEmpty()) {
      item(key = "empty") {
        EmptyHint(stringResource(R.string.spaces_empty))
      }
    }
    collapsibleRows(sections, list, collapsed, onToggle = collapsing::toggle, canBrowse = canBrowse)
    if (canBrowse) {
      item(key = "browse") {
        EntryRow(Icons.Filled.Add, stringResource(R.string.browse_channels)) {
          onNavigate(maybeSpace?.let { "browse?space=${it.id}" } ?: "browse")
        }
      }
    }
  }
}

/**
 * DMs: the DMs and group DMs (outside spaces) with their filters. The conversations start right
 * below the bar; people are behind the new-message button.
 */
@Composable
private fun DirectTab(conversations: List<ConversationItem>, list: ListState, listState: LazyListState, onNavigate: (String) -> Unit) {
  var dmFilter by rememberSaveable { mutableStateOf("") }
  var homeChip by rememberSaveable { mutableStateOf(HomeFilter.All) }
  if (conversations.none { it.isDirect }) {
    Column(Modifier.fillMaxSize()) {
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
  LazyColumn(Modifier.fillMaxSize(), state = listState, contentPadding = PaddingValues(bottom = floatingTabBarClearance())) {
    item(key = "filter") {
      FilterBar(dmFilter, onChange = { dmFilter = it }) {
        HomeChips(homeChip, onSelect = { homeChip = if (homeChip == it) HomeFilter.All else it })
      }
    }
    if (sections.isEmpty()) item(key = "no-match") { EmptyHint(stringResource(R.string.dm_filter_empty)) }
    conversationRows(sections, list, showsSpace = true)
  }
}

/**
 * The list layout's Home (#1151): the global entries, then the DMs and the channels of all spaces
 * in one list (a `LazyColumn`, so only what is on screen is built), and every space at its end,
 * also those without joined channels, with their views and menus (#1526).
 */
@Composable
private fun ListTab(
  conversations: List<ConversationItem>,
  spaces: List<HomeSpace>,
  list: ListState,
  listState: LazyListState,
  onNavigate: (String) -> Unit,
  canBrowse: Boolean,
  quickAccess: QuickAccess,
) {
  var filter by rememberSaveable { mutableStateOf("") }
  var chip by rememberSaveable { mutableStateOf(HomeFilter.All) }
  val sections = listSections(conversations, list.lastActivity, filter, chip)
  val isFiltering = filter.isNotBlank() || chip != HomeFilter.All
  LazyColumn(Modifier.fillMaxSize(), state = listState, contentPadding = PaddingValues(bottom = floatingTabBarClearance())) {
    item(key = "quick-access") { QuickAccessRow(quickAccess, onNavigate) }
    item(key = "filter") {
      FilterBar(filter, onChange = { filter = it }) {
        HomeChips(chip, showsChannels = true, onSelect = { chip = if (chip == it) HomeFilter.All else it })
      }
    }
    if (sections.isEmpty()) item(key = "no-match") { EmptyHint(stringResource(R.string.dm_filter_empty)) }
    conversationRows(sections, list, showsSpace = true)
    if (!isFiltering) {
      if (spaces.isNotEmpty()) item(key = "spaces-title") { SectionHeader(SectionTitle.Spaces) }
      items(spaces, key = { "space-${it.id}" }) { space -> ListedSpaceRow(space, list) }
      if (canBrowse) {
        item(key = "browse") { EntryRow(Icons.Filled.Add, stringResource(R.string.browse_channels)) { onNavigate("browse") } }
      }
    }
  }
}

/**
 * The list layout grouped by space (#1345, #1526): the DMs, then each space's channels in a
 * section the user can collapse, as Home of the spaces layout has them, and spaces without joined
 * channels too. Each space's header has its views and menu behind a visible button.
 */
@Composable
private fun ListBySpaceTab(
  conversations: List<ConversationItem>,
  spaces: List<HomeSpace>,
  list: ListState,
  listState: LazyListState,
  onNavigate: (String) -> Unit,
  canBrowse: Boolean,
  quickAccess: QuickAccess,
) {
  var filter by rememberSaveable { mutableStateOf("") }
  var chip by rememberSaveable { mutableStateOf(HomeFilter.All) }
  val isFiltering = filter.isNotBlank() || chip != HomeFilter.All
  val grouped = listSections(conversations, list.lastActivity, filter, chip, bySpace = true)
  // While filtering, only matches count: no empty spaces, nothing collapsed away.
  val sections = if (isFiltering) grouped else withEmptySpaces(grouped, spaces)
  val context = LocalContext.current
  val collapsing = remember { CollapsedSections.of(context) }
  val collapsed by collapsing.collapsed.collectAsState()
  LazyColumn(Modifier.fillMaxSize(), state = listState, contentPadding = PaddingValues(bottom = floatingTabBarClearance())) {
    item(key = "quick-access") { QuickAccessRow(quickAccess, onNavigate) }
    item(key = "filter") {
      FilterBar(filter, onChange = { filter = it }) {
        HomeChips(chip, showsChannels = true, onSelect = { chip = if (chip == it) HomeFilter.All else it })
      }
    }
    if (sections.isEmpty()) item(key = "no-match") { EmptyHint(stringResource(R.string.dm_filter_empty)) }
    collapsibleRows(sections, list, if (isFiltering) emptySet() else collapsed, onToggle = collapsing::toggle, canBrowse = canBrowse)
    if (canBrowse && !isFiltering) {
      item(key = "browse") { EntryRow(Icons.Filled.Add, stringResource(R.string.browse_channels)) { onNavigate("browse") } }
    }
  }
}

/** A space under the list layout's list: its mark, name and news; a tap opens its views and menu. */
@Composable
private fun ListedSpaceRow(space: HomeSpace, list: ListState) {
  var isOpen by remember { mutableStateOf(false) }
  val menuLabel = stringResource(R.string.space_options, space.name)
  Box(Modifier.fillMaxWidth()) {
    ListItem(
      headlineContent = { Text(space.name, maxLines = 1, overflow = TextOverflow.Ellipsis) },
      leadingContent = { SpaceTile(space.id, space.name, space.maybeImageVersion) },
      trailingContent = {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(Spacing.s)) {
          SpaceBadge(space)
          Icon(Icons.Filled.MoreVert, contentDescription = null, tint = MaterialTheme.colorScheme.onSurfaceVariant)
        }
      },
      modifier = Modifier.clickable(onClickLabel = menuLabel) { isOpen = true },
    )
    list.spaceMenu(space.id, space.name, isOpen) { isOpen = false }
  }
}

/**
 * Activity: threads and meetings, then the requests to join that wait for the user's decision,
 * then every conversation that waits for the user. Saved items, drafts and scheduled messages are
 * Home's quick-access cards.
 */
@Composable
private fun ActivityTab(
  conversations: List<ConversationItem>,
  list: ListState,
  onNavigate: (String) -> Unit,
  joinRequestCount: Int,
  joinRequests: @Composable () -> Unit,
  guestNoticeCount: Int,
  guestNotices: @Composable () -> Unit,
  isSyncing: Boolean,
) {
  val waiting = activityConversations(conversations, list.lastActivity)
  Column(Modifier.fillMaxSize()) {
    EntryRow(Icons.AutoMirrored.Filled.List, stringResource(R.string.threads_title)) { onNavigate("threads") }
    EntryRow(Icons.Filled.DateRange, stringResource(R.string.home_entry_meetings)) { onNavigate("meetings") }
    HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
    if (waiting.isEmpty() && joinRequestCount == 0 && guestNoticeCount == 0 && isSyncing) {
      ListSkeleton()
    } else if (waiting.isEmpty() && joinRequestCount == 0 && guestNoticeCount == 0) {
      RemainingEmptyState(
        icon = Icons.Filled.Done,
        title = stringResource(R.string.activity_empty_title),
        text = stringResource(R.string.activity_empty),
        maybeAction = null,
      )
    } else {
      LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(bottom = floatingTabBarClearance())) {
        if (guestNoticeCount > 0) item(key = "guest-notices") { guestNotices() }
        if (joinRequestCount > 0) item(key = "join-requests") { joinRequests() }
        conversationRows(listOf(Section(SectionTitle.Activity, waiting)), list, hasHeader = false)
      }
    }
  }
}

/**
 * [showsSpace]: name the space after a channel's name, where the list is not that space's own
 * (#1152). A space's section names it in its header, so its rows don't (#1345).
 */
private fun LazyListScope.conversationRows(
  sections: List<Section>,
  list: ListState,
  hasHeader: Boolean = true,
  showsSpace: Boolean = false,
) {
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
        showsSpace = showsSpace && section.title !is SectionTitle.Space,
        callCount = list.runningCalls[conversation.id] ?: 0,
        typing = list.typing[conversation.id].orEmpty(),
        onClick = { list.onOpen(conversation.id) },
        onLongPress = { list.onLongPress(conversation.id) },
        menu = { isOpen, onDismiss -> list.conversationMenu(conversation, isOpen, onDismiss) },
      )
    }
  }
}

/**
 * Home's sections (#1488): each header collapses its section; a collapsed one keeps what is new
 * and the open conversation ([visibleRows]), so nothing new hides behind a chevron.
 */
private fun LazyListScope.collapsibleRows(
  sections: List<Section>,
  list: ListState,
  collapsed: Set<String>,
  onToggle: (String) -> Unit,
  canBrowse: Boolean = false,
) {
  for (section in sections) {
    val maybeKey = section.collapseKey()
    val isCollapsed = maybeKey != null && maybeKey in collapsed
    item(key = "section-${section.title}", contentType = "header") {
      CollapsibleHeader(section, list, isCollapsed, onToggle = { maybeKey?.let(onToggle) })
    }
    // A space without joined channels says so, and where to find some (#1526).
    val maybeEmptySpaceId = section.spaceId().takeIf { section.conversations.isEmpty() && !isCollapsed }
    if (maybeEmptySpaceId != null) {
      item(key = "empty-${section.title}", contentType = "empty") {
        Column(Modifier.animateItem()) {
          EmptyHint(stringResource(R.string.space_no_channels))
          // Home of the spaces layout has the same entry right below.
          if (canBrowse && section.title is SectionTitle.Space) {
            EntryRow(Icons.Filled.Add, stringResource(R.string.browse_channels)) { list.onBrowseSpace(maybeEmptySpaceId) }
          }
        }
      }
    }
    val rows = section.visibleRows(isCollapsed, list.maybeSelectedId)
    items(rows, key = { "${section.title}-${it.id}" }, contentType = { "row" }) { conversation ->
      SwipeableConversationRow(
        actions = list.actionsFor(conversation, list.previews[conversation.id]),
        conversation = conversation,
        maybeEntry = list.previews[conversation.id],
        isSelected = conversation.id == list.maybeSelectedId,
        isChecked = conversation.id in list.selection,
        isSelecting = list.selection.isNotEmpty(),
        showsSpace = section.title is SectionTitle.Favorites,
        callCount = list.runningCalls[conversation.id] ?: 0,
        typing = list.typing[conversation.id].orEmpty(),
        onClick = { list.onOpen(conversation.id) },
        onLongPress = { list.onLongPress(conversation.id) },
        menu = { isOpen, onDismiss -> list.conversationMenu(conversation, isOpen, onDismiss) },
        modifier = Modifier.animateItem(),
      )
    }
  }
}

/**
 * A Home section's header: the chevron and the title; a tap collapses or opens the section. A
 * space's header also has its picture, a button to its views and its menu on a long-press (#1105).
 */
@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun CollapsibleHeader(section: Section, list: ListState, isCollapsed: Boolean, onToggle: () -> Unit) {
  val haptics = LocalHapticFeedback.current
  val title = section.title
  // Only a space's own section has its menu here; Home of the spaces layout has it in the header.
  val maybeSpaceId = section.spaceId().takeIf { title is SectionTitle.Space }
  val name = sectionTitleText(title)
  var isMenuOpen by remember { mutableStateOf(false) }
  // A quarter turn, short and without overshoot: it follows the tap, it doesn't perform.
  val rotation by animateFloatAsState(
    targetValue = if (isCollapsed) -90f else 0f,
    animationSpec = spring(stiffness = Spring.StiffnessMediumLow),
    label = "chevron",
  )
  val toggleLabel = stringResource(if (isCollapsed) R.string.section_expand else R.string.section_collapse, name)
  Box(Modifier.fillMaxWidth()) {
    Row(
      Modifier
        .fillMaxWidth()
        .combinedClickable(
          onClickLabel = toggleLabel,
          onClick = onToggle,
          onLongClickLabel = maybeSpaceId?.let { stringResource(R.string.entry_space_menu) },
          onLongClick = maybeSpaceId?.let {
            {
              haptics.performHapticFeedback(HapticFeedbackType.LongPress)
              isMenuOpen = true
            }
          },
        )
        .semantics {
          heading()
          contentDescription = name
          stateDescription = toggleLabel
        }
        .padding(start = Spacing.m, end = if (maybeSpaceId != null) 56.dp else Spacing.m, top = Spacing.m, bottom = Spacing.xs)
        .heightIn(min = 32.dp),
      verticalAlignment = Alignment.CenterVertically,
      horizontalArrangement = Arrangement.spacedBy(Spacing.s),
    ) {
      Icon(
        Icons.Filled.KeyboardArrowDown,
        contentDescription = null,
        tint = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.size(20.dp).graphicsLayer { rotationZ = rotation },
      )
      val maybeVersion = (title as? SectionTitle.Space)?.maybeImageVersion
        ?: section.conversations.firstNotNullOfOrNull { it.maybeSpaceImageVersion }
      if (maybeSpaceId != null && maybeVersion != null) SpacePicture(maybeSpaceId, maybeVersion)
      Text(
        name,
        style = MaterialTheme.typography.titleSmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        maxLines = 1,
        overflow = TextOverflow.Ellipsis,
      )
    }
    if (maybeSpaceId != null) {
      // The space's views and actions behind a visible button (#1526), not only a long-press.
      Box(Modifier.align(Alignment.CenterEnd).padding(end = Spacing.xs)) {
        IconButton(onClick = { isMenuOpen = true }) {
          Icon(
            Icons.Filled.MoreVert,
            contentDescription = stringResource(R.string.space_options, name),
            tint = MaterialTheme.colorScheme.onSurfaceVariant,
          )
        }
        list.spaceMenu(maybeSpaceId, name, isMenuOpen) { isMenuOpen = false }
      }
    }
  }
}

/** What the quick-access cards on top of Home count, from state the list has anyway (#1466). */
internal data class QuickAccess(
  val draftCount: Int = 0,
  /** `null` while scheduling is off for the instance: no card then. */
  val maybeScheduledCount: Int? = null,
  val needsAttention: Boolean = false,
)

/** Drafts from the conversations' draft previews, scheduled messages from the banner's read. */
internal fun quickAccess(conversations: List<ConversationItem>, maybeScheduled: ScheduledOverview?): QuickAccess =
  QuickAccess(
    draftCount = conversations.count { it.maybeDraftPreview != null },
    maybeScheduledCount = maybeScheduled?.takeIf { it.policy.isEnabled || it.messages.isNotEmpty() }?.messages?.size,
    needsAttention = maybeScheduled?.messages?.any { it.needsAttention } == true,
  )

/**
 * The quick-access cards (#1488): Threads, Saved, Drafts and Scheduled side by side, each with an
 * icon, a title and a one-line status; a card with something waiting is tinted.
 */
@Composable
private fun QuickAccessRow(quickAccess: QuickAccess, onNavigate: (String) -> Unit) {
  Row(
    Modifier
      .fillMaxWidth()
      .horizontalScroll(rememberScrollState())
      .padding(horizontal = Spacing.l, vertical = Spacing.s),
    horizontalArrangement = Arrangement.spacedBy(Spacing.s),
  ) {
    QuickAccessCard(
      icon = { Icon(Icons.AutoMirrored.Filled.List, contentDescription = null) },
      title = stringResource(R.string.home_entry_threads),
      status = stringResource(R.string.quick_threads_status),
      isHighlighted = false,
    ) { onNavigate("threads") }
    QuickAccessCard(
      icon = { Icon(painterResource(R.drawable.ic_bookmark), contentDescription = null) },
      title = stringResource(R.string.home_entry_saved),
      status = stringResource(R.string.quick_saved_status),
      isHighlighted = false,
    ) { onNavigate("saved") }
    QuickAccessCard(
      icon = { Icon(Icons.Filled.Edit, contentDescription = null) },
      title = stringResource(R.string.home_entry_drafts),
      status = if (quickAccess.draftCount > 0) {
        pluralStringResource(R.plurals.quick_drafts_count, quickAccess.draftCount, quickAccess.draftCount)
      } else {
        stringResource(R.string.quick_drafts_none)
      },
      isHighlighted = quickAccess.draftCount > 0,
    ) { onNavigate("drafts") }
    quickAccess.maybeScheduledCount?.let { count ->
      QuickAccessCard(
        icon = { Icon(painterResource(R.drawable.ic_schedule), contentDescription = null) },
        title = stringResource(R.string.home_entry_scheduled),
        status = when {
          quickAccess.needsAttention -> stringResource(R.string.quick_scheduled_attention)
          count > 0 -> pluralStringResource(R.plurals.quick_scheduled_count, count, count)
          else -> stringResource(R.string.quick_scheduled_none)
        },
        isHighlighted = count > 0 || quickAccess.needsAttention,
      ) { onNavigate("scheduled") }
    }
  }
}

@Composable
private fun QuickAccessCard(
  icon: @Composable () -> Unit,
  title: String,
  status: String,
  isHighlighted: Boolean,
  onClick: () -> Unit,
) {
  val colors = MaterialTheme.colorScheme
  Card(
    onClick = onClick,
    shape = RoundedCornerShape(16.dp),
    colors = CardDefaults.cardColors(
      containerColor = if (isHighlighted) colors.primaryContainer else colors.surfaceContainerHigh,
      contentColor = if (isHighlighted) colors.onPrimaryContainer else colors.onSurface,
    ),
    modifier = Modifier.width(QUICK_CARD_WIDTH).semantics(mergeDescendants = true) {},
  ) {
    Column(Modifier.padding(Spacing.m), verticalArrangement = Arrangement.spacedBy(Spacing.xs)) {
      Box(Modifier.size(20.dp), contentAlignment = Alignment.Center) { icon() }
      Text(title, style = MaterialTheme.typography.titleSmall, maxLines = 1, overflow = TextOverflow.Ellipsis)
      Text(
        status,
        style = MaterialTheme.typography.bodySmall,
        color = if (isHighlighted) colors.onPrimaryContainer else colors.onSurfaceVariant,
        fontWeight = if (isHighlighted) FontWeight.SemiBold else FontWeight.Normal,
        maxLines = 1,
        overflow = TextOverflow.Ellipsis,
      )
    }
  }
}

/** The rounded top corners of the neutral card the main screen's lists sit on. */
private val CONTENT_CARD_RADIUS = 20.dp

/**
 * The top app bar of the main screen: the server's brand (logo and name) and the user's avatar, on
 * the theme's header colour (#202), which also fills the status bar behind it.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun BrandTopBar(
  brand: MainBrand,
  accountUserId: String,
  accountName: String,
  onSearch: () -> Unit,
  onAccount: () -> Unit,
  maybeSpaceHeader: SpaceHeader? = null,
  spaceMenu: @Composable (String, String, Boolean, () -> Unit) -> Unit = { _, _, _, _ -> },
) {
  val colors = LocalTeamCraftColors.current
  val isDarkHeader = colors.chrome.luminance() < 0.5f
  TopAppBar(
    modifier = if (isDarkHeader) Modifier.darkStatusBarBackdrop() else Modifier,
    title = {
      val header = maybeSpaceHeader
      if (header != null) {
        SpaceSwitcherTitle(header.space, header.hasNewElsewhere, header.onSwitch)
      } else {
        BrandTitle(brand)
      }
    },
    actions = {
      IconButton(onClick = onSearch) {
        Icon(Icons.Filled.Search, contentDescription = stringResource(R.string.search))
      }
      maybeSpaceHeader?.let { header ->
        // The selected space's views, browsing and actions (#1526), next to its name.
        var isMenuOpen by remember(header.space.id) { mutableStateOf(false) }
        Box {
          IconButton(onClick = { isMenuOpen = true }) {
            Icon(Icons.Filled.MoreVert, contentDescription = stringResource(R.string.space_options, header.space.name))
          }
          spaceMenu(header.space.id, header.space.name, isMenuOpen) { isMenuOpen = false }
        }
      }
      val description = stringResource(R.string.account_menu, accountName)
      IconButton(onClick = onAccount, modifier = Modifier.semantics { contentDescription = description }) {
        AccountAvatar(accountUserId, accountName, size = 32.dp)
      }
    },
    windowInsets = TopBarInsets,
    colors = TopAppBarDefaults.topAppBarColors(
      containerColor = colors.chrome,
      titleContentColor = colors.onChrome,
      actionIconContentColor = colors.onChrome,
    ),
  )
}

/** The brand's logo and name, the header's title outside the spaces layout's Home. */
@Composable
private fun BrandTitle(brand: MainBrand) {
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
}

/** One entry as a full-width row (Activity, Spaces). */
@Composable
private fun EntryRow(icon: ImageVector, label: String, onClick: () -> Unit) {
  ListItem(
    headlineContent = { Text(label, style = MaterialTheme.typography.titleSmall) },
    leadingContent = { Icon(icon, contentDescription = null, tint = MaterialTheme.colorScheme.primary) },
    // On the list's own background, as the conversation rows above it.
    colors = ListItemDefaults.colors(containerColor = Color.Transparent),
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
internal fun EmptyState(icon: ImageVector, title: String, text: String, maybeAction: Pair<String, () -> Unit>?) {
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
private fun HomeChips(selected: HomeFilter, showsChannels: Boolean = false, onSelect: (HomeFilter) -> Unit) {
  Row(
    Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()).padding(horizontal = Spacing.l),
    horizontalArrangement = Arrangement.spacedBy(Spacing.s),
  ) {
    val chips = listOf(
      HomeFilter.Unread to R.string.home_filter_unread,
      HomeFilter.People to R.string.home_filter_people,
      HomeFilter.Groups to R.string.home_filter_groups,
    ) + if (showsChannels) listOf(HomeFilter.Channels to R.string.channels) else emptyList()
    for ((chip, label) in chips) {
      FilterChip(selected = chip == selected, onClick = { onSelect(chip) }, label = { Text(stringResource(label)) })
    }
  }
}

/**
 * Filtering the list: a compact field with the chips under it, about half the height the field and
 * the entries above it took before, so the first conversations show without scrolling.
 */
@Composable
private fun FilterBar(value: String, onChange: (String) -> Unit, chips: @Composable () -> Unit) {
  Column(Modifier.fillMaxWidth().padding(top = Spacing.xs, bottom = Spacing.xs), verticalArrangement = Arrangement.spacedBy(Spacing.s)) {
    CompactSearchField(
      value = value,
      onChange = onChange,
      placeholder = stringResource(R.string.dm_filter),
      modifier = Modifier.padding(horizontal = Spacing.l),
      isFieldDeferred = true,
    )
    chips()
  }
}

@Composable
internal fun EmptyHint(text: String) {
  Text(
    text,
    style = MaterialTheme.typography.bodyMedium,
    color = MaterialTheme.colorScheme.onSurfaceVariant,
    textAlign = TextAlign.Center,
    modifier = Modifier.fillMaxWidth().padding(32.dp),
  )
}

@Composable
internal fun SectionHeader(title: SectionTitle) {
  val text = sectionTitleText(title)
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

@Composable
private fun sectionTitleText(title: SectionTitle): String =
  when (title) {
    SectionTitle.Favorites -> stringResource(R.string.favorites)
    SectionTitle.Archived -> stringResource(R.string.section_archived)
    SectionTitle.Channels -> stringResource(R.string.channels)
    SectionTitle.DirectMessages -> stringResource(R.string.direct_messages)
    SectionTitle.Everything -> stringResource(R.string.list_conversations)
    SectionTitle.Spaces -> stringResource(R.string.nav_spaces)
    SectionTitle.Activity -> stringResource(R.string.nav_activity)
    SectionTitle.Notes -> stringResource(R.string.notes_title)
    SectionTitle.Recent -> stringResource(R.string.share_recent)
    SectionTitle.UnreadDirect -> stringResource(R.string.section_unread_direct)
    is SectionTitle.Space -> title.name
    is SectionTitle.SpaceChannels -> stringResource(R.string.channels)
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
  showsSpace: Boolean,
  callCount: Int,
  typing: List<String>,
  onClick: () -> Unit,
  onLongPress: () -> Unit,
  menu: @Composable (Boolean, () -> Unit) -> Unit,
  modifier: Modifier = Modifier,
) {
  val haptics = LocalHapticFeedback.current
  val scope = rememberCoroutineScope()
  // While rows are being selected, a long-press keeps toggling; otherwise it opens the menu.
  var isMenuOpen by remember { mutableStateOf(false) }
  val state = rememberSwipeToDismissBoxState()
  SwipeToDismissBox(
    state = state,
    modifier = modifier,
    gesturesEnabled = !isSelecting,
    enableDismissFromStartToEnd = actions.toggleRead != null,
    backgroundContent = { SwipeBackground(state.dismissDirection, conversation, isUnread = actions.isUnread) },
    onDismiss = { direction ->
      when (direction) {
        SwipeToDismissBoxValue.StartToEnd -> actions.toggleRead?.invoke()
        SwipeToDismissBoxValue.EndToStart -> actions.toggleFavorite()
        SwipeToDismissBoxValue.Settled -> Unit
      }
      // The swipe did its action (Material haptics: confirm, not a long press).
      haptics.performHapticFeedback(HapticFeedbackType.Confirm)
      // Never removed: the row snaps back.
      scope.launch { state.reset() }
    },
  ) {
    Box {
      ConversationRow(
        conversation,
        maybeEntry,
        actions,
        isSelected,
        isChecked,
        showsSpace,
        callCount,
        typing,
        onClick,
        onLongPress = { if (isSelecting) onLongPress() else isMenuOpen = true },
      )
      menu(isMenuOpen) { isMenuOpen = false }
    }
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
  showsSpace: Boolean,
  callCount: Int,
  typing: List<String>,
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
        .padding(horizontal = 16.dp, vertical = 8.dp),
      verticalAlignment = Alignment.CenterVertically,
    ) {
      if (isChecked) CheckedAvatar() else ConversationAvatar(conversation)
      Column(Modifier.weight(1f).padding(start = 12.dp)) {
        TitleLine(conversation, maybeEntry, maybeSpaceName = conversation.maybeSpaceName.takeIf { showsSpace }, callCount = callCount)
        PreviewLine(conversation, maybeEntry, typing)
      }
    }
  }
}

/** A call runs here: a green phone and how many are in it, before the unread count. */
@Composable
private fun RunningCallMark(count: Int) {
  val description = pluralStringResource(R.plurals.call_running_people, count, count)
  Row(
    Modifier
      .padding(start = 8.dp)
      .background(CALL_GREEN, CircleShape)
      .padding(horizontal = 6.dp, vertical = 1.dp)
      .semantics(mergeDescendants = true) { contentDescription = description },
    verticalAlignment = Alignment.CenterVertically,
  ) {
    Icon(Icons.Filled.Call, contentDescription = null, tint = Color.White, modifier = Modifier.size(12.dp))
    Text(count.toString(), style = MaterialTheme.typography.labelSmall, color = Color.White, modifier = Modifier.padding(start = 2.dp))
  }
}

/** The BOT marker after the name of a DM with a bot or integration account (as on web). */
@Composable
private fun BotBadge() {
  Text(
    stringResource(R.string.badge_bot),
    modifier = Modifier
      .padding(start = 6.dp)
      .background(MaterialTheme.colorScheme.secondaryContainer, RoundedCornerShape(4.dp))
      .padding(horizontal = 4.dp, vertical = 1.dp),
    style = MaterialTheme.typography.labelSmall,
    color = MaterialTheme.colorScheme.onSecondaryContainer,
    maxLines = 1,
  )
}

private val CALL_GREEN = Color(0xFF2E9E5B)

@Composable
private fun TitleLine(conversation: ConversationItem, maybeEntry: ChatListEntry?, maybeSpaceName: String? = null, callCount: Int = 0) {
  val context = LocalContext.current
  val isUnread = conversation.unreadCount > 0u
  Row(verticalAlignment = Alignment.CenterVertically) {
    Text(
      conversation.listName(stringResource(R.string.notes_title)),
      // With the space after it, both share the line (same row height as the others); the space
      // gets the smaller share, so the name is cut last (#1346).
      modifier = if (maybeSpaceName != null) Modifier.weight(2f, fill = false) else Modifier.weight(1f),
      maxLines = 1,
      overflow = TextOverflow.Ellipsis,
      style = MaterialTheme.typography.titleSmall,
      fontWeight = if (isUnread) FontWeight.Bold else FontWeight.Normal,
      color = if (conversation.isMuted) MaterialTheme.colorScheme.onSurfaceVariant else MaterialTheme.colorScheme.onSurface,
    )
    if (conversation.isWithBot()) BotBadge()
    if (maybeSpaceName != null) {
      Text(
        maybeSpaceName,
        modifier = Modifier.weight(1f).padding(start = 6.dp),
        maxLines = 1,
        overflow = TextOverflow.Ellipsis,
        style = MaterialTheme.typography.labelSmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
      )
    }
    if (conversation.isMuted) {
      val mutedLabel = stringResource(R.string.chat_muted)
      Icon(
        painterResource(R.drawable.ic_notifications_off),
        contentDescription = mutedLabel,
        tint = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.padding(start = 4.dp).size(14.dp),
      )
    }
    if (callCount > 0) RunningCallMark(callCount)
    rowBadge(conversation.unreadCount, conversation.mentionCount, conversation.isMuted)?.let { badge ->
      RowCount(badge, conversation.isMuted)
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
private fun PreviewLine(conversation: ConversationItem, maybeEntry: ChatListEntry?, typing: List<String>) {
  // Someone types: that stands in for the preview and the draft, until they stop (#882).
  typingLine(typing)?.let { line ->
    TypingPreview(typingText(line, isDirect = conversation.kind == ConversationKind.DIRECT))
    return
  }
  // A draft stands in for the newest message: it's what the user owes this conversation (#1220).
  conversation.maybeDraftPreview?.let { preview ->
    DraftLine(preview)
    return
  }
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
  }
}

/** "typing" or "Alice is typing" in the preview's place, after three pulsing dots. */
@Composable
private fun TypingPreview(text: String) {
  Row(verticalAlignment = Alignment.CenterVertically) {
    TypingDots()
    Text(
      text,
      modifier = Modifier.weight(1f).padding(start = 6.dp),
      maxLines = 1,
      overflow = TextOverflow.Ellipsis,
      style = MaterialTheme.typography.bodyMedium,
      fontStyle = FontStyle.Italic,
      color = MaterialTheme.colorScheme.primary,
    )
  }
}

@Composable
private fun previewText(preview: ChatPreview): String {
  val body = when (val content = preview.content) {
    is PreviewContent.Text -> content.text
    PreviewContent.Deleted -> stringResource(R.string.chat_preview_deleted)
    PreviewContent.Event -> stringResource(R.string.chat_preview_event)
    is PreviewContent.Placeholder -> stringResource(
      when (content.state) {
        "waiting" -> R.string.message_placeholder_waiting
        "failed" -> R.string.message_placeholder_failed
        else -> R.string.message_placeholder_not_available
      },
    )
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

/**
 * A round marker: the DM peer's picture, or the conversation's initial (people) or its kind
 * (channels); the notes show a bookmark.
 */
@Composable
internal fun ConversationAvatar(conversation: ConversationItem) {
  if (conversation.isNotesToSelf) {
    NotesAvatar()
    return
  }
  val isPerson = conversation.kind == ConversationKind.DIRECT || conversation.kind == ConversationKind.GROUP
  if (conversation.kind == ConversationKind.PUBLIC || conversation.kind == ConversationKind.PRIVATE) {
    ChannelAvatar(isPrivate = conversation.kind == ConversationKind.PRIVATE)
    return
  }
  val label = conversation.name.firstOrNull()?.uppercase() ?: "?"
  Box {
    UserAvatar(conversation.maybePeerId, Modifier.size(AVATAR_SIZE)) {
      Box(
        Modifier
          .size(AVATAR_SIZE)
          .background(
            if (isPerson) conversationAvatarColor(conversation) else MaterialTheme.colorScheme.surfaceVariant,
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
    }
    conversation.maybePeerPresence?.let { presence ->
      val description = stringResource(presenceLabel(presence))
      Box(
        Modifier
          .align(Alignment.BottomEnd)
          .size(12.dp)
          .background(MaterialTheme.colorScheme.background, CircleShape)
          .semantics { contentDescription = description },
        contentAlignment = Alignment.Center,
      ) { PresenceDot(presence, size = 8.dp) }
    }
  }
}

/**
 * A channel's marker: a rounded square, not a circle, so channels and people tell apart at a glance;
 * `#` for a public channel, a lock for a private one (the sidebar's markers on desktop and web).
 */
@Composable
private fun ChannelAvatar(isPrivate: Boolean) {
  val colors = MaterialTheme.colorScheme
  Box(
    Modifier.size(AVATAR_SIZE).background(colors.surfaceContainerHighest, RoundedCornerShape(12.dp)),
    contentAlignment = Alignment.Center,
  ) {
    if (isPrivate) {
      Icon(Icons.Filled.Lock, contentDescription = null, tint = colors.onSurfaceVariant, modifier = Modifier.size(20.dp))
    } else {
      Text("#", style = MaterialTheme.typography.titleMedium.copy(fontWeight = FontWeight.SemiBold), color = colors.onSurfaceVariant)
    }
  }
}

@Composable
private fun NotesAvatar() {
  Box(
    Modifier.size(AVATAR_SIZE).background(MaterialTheme.colorScheme.primaryContainer, CircleShape),
    contentAlignment = Alignment.Center,
  ) {
    Icon(
      painterResource(R.drawable.ic_bookmark),
      contentDescription = null,
      tint = MaterialTheme.colorScheme.onPrimaryContainer,
      modifier = Modifier.size(22.dp),
    )
  }
}

/**
 * The unread number next to a conversation's name (#897): count and tone come from the shared rule
 * (`rowBadge`), red for a conversation that notifies and for mentions, grey for a muted one. The
 * minimum width keeps the name from shifting while the count changes.
 */
@Composable
internal fun RowCount(badge: RowBadge, isMuted: Boolean) {
  val colors = LocalTeamCraftColors.current
  val description = pluralStringResource(
    // A red badge in a muted conversation counts only its mentions.
    if (isMuted && badge.tone == RowBadgeTone.RED) R.plurals.chat_mentions else R.plurals.chat_unread,
    badge.count.toInt(),
    badge.count.toInt(),
  )
  Badge(
    containerColor = if (badge.tone == RowBadgeTone.RED) colors.badge else MaterialTheme.colorScheme.surfaceContainerHighest,
    contentColor = if (badge.tone == RowBadgeTone.RED) colors.chromeText else MaterialTheme.colorScheme.onSurfaceVariant,
    modifier = Modifier
      .padding(start = 8.dp)
      .widthIn(min = 24.dp)
      .semantics { contentDescription = description },
  ) {
    Text(badge.label, maxLines = 1, style = MaterialTheme.typography.labelSmall)
  }
}

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
    windowInsets = TopBarInsets,
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

/** The user's own avatar: their picture, or the initial on their hue, as for other people. */
@Composable
internal fun AccountAvatar(userId: String, name: String, size: Dp) {
  UserAvatar(userId, Modifier.size(size)) {
    Box(Modifier.size(size).background(avatarColor(userId, name), CircleShape), contentAlignment = Alignment.Center) {
      Text(
        name.firstOrNull()?.uppercase() ?: "?",
        style = MaterialTheme.typography.titleSmall,
        color = Color.White,
        modifier = Modifier.clearAndSetSemantics {},
      )
    }
  }
}
