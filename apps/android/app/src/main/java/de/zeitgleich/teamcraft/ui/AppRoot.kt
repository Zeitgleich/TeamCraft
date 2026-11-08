package de.zeitgleich.teamcraft.ui

import android.content.Intent
import androidx.compose.runtime.Composable
import androidx.compose.ui.unit.dp
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.Modifier
import androidx.compose.ui.Alignment
import androidx.compose.runtime.setValue
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.key
import androidx.compose.material3.VerticalDivider
import androidx.compose.material3.Text
import androidx.compose.material3.MaterialTheme
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.background
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.remember
import androidx.navigation.NavType
import androidx.navigation.compose.NavHost
import androidx.navigation.compose.composable
import androidx.navigation.compose.rememberNavController
import androidx.navigation.navArgument
import kotlinx.coroutines.launch
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.ui.screens.BrowseScreen
import de.zeitgleich.teamcraft.ui.screens.ChatListScreen
import de.zeitgleich.teamcraft.ui.screens.ConfirmServerScreen
import de.zeitgleich.teamcraft.ui.screens.ServerEntryScreen
import de.zeitgleich.teamcraft.ui.screens.ReportProblemScreen
import de.zeitgleich.teamcraft.ui.screens.DraftsAndSentScreen
import de.zeitgleich.teamcraft.ui.screens.FilesScreen
import de.zeitgleich.teamcraft.ui.screens.OpenedDraft
import de.zeitgleich.teamcraft.ui.screens.MeetingsScreen
import uniffi.teamcraft_ffi.AppLinkPlace
import uniffi.teamcraft_ffi.OpenedAppLink
import uniffi.teamcraft_ffi.OpenedAppLinkStep
import uniffi.teamcraft_ffi.openAppLink
import de.zeitgleich.teamcraft.ui.screens.SavedScreen
import de.zeitgleich.teamcraft.ui.screens.ScheduledScreen
import de.zeitgleich.teamcraft.ui.screens.SpaceViewsScreen
import de.zeitgleich.teamcraft.ui.screens.ThreadsScreen
import de.zeitgleich.teamcraft.ui.screens.ConversationScreen
import de.zeitgleich.teamcraft.ui.screens.DoNotDisturbScreen
import de.zeitgleich.teamcraft.ui.screens.NotificationDiagnosticsScreen
import de.zeitgleich.teamcraft.ui.screens.PushBatteryScreen
import de.zeitgleich.teamcraft.ui.screens.EnrollScreen
import de.zeitgleich.teamcraft.ui.screens.LinkRequestPrompts
import de.zeitgleich.teamcraft.ui.screens.LinkThisDeviceScreen
import de.zeitgleich.teamcraft.ui.screens.LoginScreen
import de.zeitgleich.teamcraft.ui.screens.MessageScreen
import de.zeitgleich.teamcraft.ui.screens.OrgRecoveryScreen
import de.zeitgleich.teamcraft.ui.screens.SearchScreen
import de.zeitgleich.teamcraft.ui.screens.SettingsScreen
import de.zeitgleich.teamcraft.ui.screens.CrashReportsNotice
import de.zeitgleich.teamcraft.ui.screens.SharePickerScreen
import de.zeitgleich.teamcraft.ui.screens.ShareWaitingNotice
import de.zeitgleich.teamcraft.ui.screens.StoreRecoveryScreen
import de.zeitgleich.teamcraft.ui.screens.ThreadScreen

@Composable
fun AppRoot(
  viewModel: MainViewModel,
  maybeConversationToOpen: String?,
  onConversationOpened: () -> Unit,
  maybeShare: ShareState?,
  maybeAppLink: String?,
  onAppLinkHandled: () -> Unit,
  onLogin: (Intent) -> Unit,
) {
  val state by viewModel.appState.collectAsState()
  val isBusy by viewModel.isBusy.collectAsState()
  val brand by viewModel.loginBrand.collectAsState()
  val brandIcons by viewModel.brandIcons.collectAsState()
  val maybeServer by viewModel.server.collectAsState()
  val isServerLocked by viewModel.isServerLocked.collectAsState()
  val isServerFromLink by viewModel.isServerFromLink.collectAsState()
  val maybeLinkServer by viewModel.linkServer.collectAsState()
  val maybeSignInPage by viewModel.signInPage.collectAsState()
  val maybeSsoIcon by viewModel.ssoIcon.collectAsState()
  val isDark = isSystemInDarkTheme()
  val scope = rememberCoroutineScope()
  val context = LocalContext.current
  // What the core makes of a link, with the server in use; a question doesn't come back after "Sign in
  // there", while the user is on the way to the account it asked about.
  var isSigningInForLink by remember(maybeAppLink) { mutableStateOf(false) }
  val maybeOpenedLink = remember(maybeAppLink, maybeLinkServer) {
    maybeLinkServer?.let { server -> maybeAppLink?.let { openAppLink(it, server.maybeOrigin, server.isLocked) } }
  }
  // The question being read stays until it is answered: a link arriving meanwhile doesn't swap it, so
  // what the user answers is what they read.
  var maybeQuestion by remember { mutableStateOf<OpenedAppLink?>(null) }
  LaunchedEffect(maybeAppLink, maybeOpenedLink, state == AppState.Starting, isSigningInForLink) {
    // Not a link after all: nothing to do, nothing to say.
    if (maybeAppLink != null && maybeLinkServer != null && maybeOpenedLink == null) onAppLinkHandled()
    if (maybeQuestion == null) {
      maybeQuestion = dueLinkQuestion(maybeOpenedLink, state == AppState.Starting, isSigningInForLink)
    }
  }
  Box(Modifier.fillMaxSize()) {
    maybeQuestion?.let { question ->
      AppLinkQuestion(
        link = question,
        onSignIn = { host ->
          maybeQuestion = null
          isSigningInForLink = true
          viewModel.submitServerFromLink(host)
        },
        onBrowser = {
          maybeQuestion = null
          question.maybeWebUrl?.let { url ->
            if (!openInBrowser(context, url)) viewModel.showFailure(R.string.app_link_no_browser)
          }
          onAppLinkHandled()
        },
        onCancel = {
          maybeQuestion = null
          onAppLinkHandled()
        },
      )
    }
    when (val current = state) {
      // The loading mark (MainActivity) stands in for this screen.
      AppState.Starting -> Unit
      is AppState.NeedsServer -> ServerEntryScreen(
        prefill = current.prefill,
        maybeError = current.maybeError,
        isLocked = current.isLocked,
        isBusy = isBusy,
        onSubmit = viewModel::submitServer,
      )
      is AppState.ConfirmServer -> ConfirmServerScreen(
        checked = current.checked,
        isFromLink = isServerFromLink,
        maybeBrand = brand,
        maybeLogo = remember(brand, isDark) { viewModel.brandLogo(isDark) },
        isBusy = isBusy,
        onContinue = {
          scope.launch {
            if (viewModel.confirmServer()) viewModel.loginIntent()?.let(onLogin)
          }
        },
        maybeOnOtherServer = if (isServerLocked) null else viewModel::useAnotherServer,
      )
      is AppState.SignedOut -> LoginScreen(
        maybeBrand = brand,
        maybeLogo = remember(brand, isDark) { viewModel.brandLogo(isDark) },
        maybeIcon = brandIcons?.forTheme(isDark),
        maybePage = maybeSignInPage,
        maybeSsoIcon = maybeSsoIcon,
        serverHost = maybeServer?.address?.host.orEmpty(),
        maybeError = current.maybeError,
        maybeErrorCode = current.maybeErrorCode,
        isBusy = isBusy,
        onSso = { scope.launch { viewModel.loginIntent()?.let(onLogin) } },
        onLocal = viewModel::signInLocal,
        maybeOnChangeServer = if (isServerLocked) null else ({ viewModel.changeServer() }),
      )
      is AppState.NeedsEnrollment -> EnrollScreen(
        hasExistingIdentity = current.hasExistingIdentity,
        maybeError = current.maybeError,
        isBusy = isBusy,
        onEnroll = viewModel::enroll,
        onLink = { viewModel.linkThisDevice() },
        onOrgRecovery = viewModel::recoverThroughOrganization,
      )
      is AppState.RecoveringThroughOrganization -> OrgRecoveryScreen(
        code = current.code,
        maybeError = current.maybeError,
        isBusy = isBusy,
        onFinish = viewModel::finishOrgRecovery,
        onCancel = viewModel::cancelLinking,
      )
      is AppState.LinkingThisDevice -> LinkThisDeviceScreen(
        state = current,
        onAskOtherDevices = viewModel::askOtherDevices,
        onScanned = viewModel::linkScannedCode,
        onCancel = viewModel::cancelLinking,
      )
      is AppState.Failed -> MessageScreen(message = current.error, onRetry = viewModel::refresh)
      is AppState.StoreUnreadable -> StoreRecoveryScreen(
        problem = current.problem,
        maybeError = current.maybeError,
        isBusy = isBusy,
        onReset = viewModel::resetStore,
        onRetry = viewModel::retryStore,
        onSignOut = viewModel::signOut,
      )
      is AppState.Ready -> {
        ReadyNavigation(
          viewModel,
          current,
          maybeConversationToOpen,
          onConversationOpened,
          maybeShare,
          maybeOpenedLink?.takeIf { it.step == OpenedAppLinkStep.Navigate },
          onAppLinkHandled,
        )
        LinkRequestPrompts(viewModel, Modifier.align(Alignment.TopCenter))
      }
    }
    // Before the signed-in screens a share waits: say so, and that it isn't lost.
    if (maybeShare != null && state != AppState.Starting && state !is AppState.Ready) {
      ShareWaitingNotice(onDiscard = viewModel::discardShare, Modifier.align(Alignment.TopCenter))
    }
  }
}

@Composable
private fun ReadyNavigation(
  viewModel: MainViewModel,
  account: AppState.Ready,
  maybeConversationToOpen: String?,
  onConversationOpened: () -> Unit,
  maybeShare: ShareState?,
  maybeOpenedLink: OpenedAppLink?,
  onAppLinkHandled: () -> Unit,
) {
  val navigation = rememberNavController()
  val scope = rememberCoroutineScope()
  val context = LocalContext.current
  var maybeMeetingLinkUrl by remember { mutableStateOf<String?>(null) }
  // A message link in a message: open its conversation (or thread) at the message, if readable here.
  val openPermalink: (String) -> Unit = { link ->
    scope.launch {
      val maybeTarget = viewModel.resolvePermalink(link)
      when {
        maybeTarget == null -> viewModel.showFailure(R.string.permalink_unavailable)
        maybeTarget.maybeThread != null ->
          navigation.navigate("thread/${maybeTarget.conversation}/${maybeTarget.maybeThread}?focus=${maybeTarget.message}")
        else -> navigation.navigate("conversation/${maybeTarget.conversation}?focus=${maybeTarget.message}")
      }
    }
  }
  LaunchedEffect(maybeOpenedLink) {
    val link = maybeOpenedLink ?: return@LaunchedEffect
    when (val place = link.place) {
      AppLinkPlace.Home, AppLinkPlace.None -> Unit
      // Only one this device knows: a link can name any ID, and the screen would stay blank.
      is AppLinkPlace.Conversation ->
        if (viewModel.conversations().any { it.id == place.conversationId }) {
          navigation.navigate("conversation/${place.conversationId}")
        } else {
          viewModel.showFailure(R.string.app_link_conversation_unavailable)
        }
      is AppLinkPlace.Permalink -> openPermalink(place.link)
      // Doesn't swap the question if one is open.
      AppLinkPlace.Meeting -> if (maybeMeetingLinkUrl == null) maybeMeetingLinkUrl = link.maybeWebUrl
    }
    onAppLinkHandled()
  }
  maybeMeetingLinkUrl?.let { url ->
    MeetingLinkQuestion(
      onMeetings = {
        maybeMeetingLinkUrl = null
        navigation.navigate("meetings") { launchSingleTop = true }
      },
      onBrowser = {
        maybeMeetingLinkUrl = null
        if (!openInBrowser(context, url)) viewModel.showFailure(R.string.app_link_no_browser)
      },
      onCancel = { maybeMeetingLinkUrl = null },
    )
  }
  LaunchedEffect(maybeConversationToOpen) {
    maybeConversationToOpen?.let {
      // A notification opened it: it has news the kept feed lacks, so the feed opens with its first
      // read, at the unread line, not at the end it showed last (issue #1425).
      viewModel.forgetFeedSeed(it)
      navigation.navigate("conversation/$it")
      onConversationOpened()
    }
  }
  val maybeShareId = (maybeShare as? ShareState.Ready)?.share?.id
  LaunchedEffect(maybeShare != null, maybeShareId) {
    if (maybeShare != null) navigation.navigate("share") { launchSingleTop = true }
  }
  CrashReportsNotice(viewModel.crashReports, onSettings = { navigation.navigate("settings/security") })
  NavHost(navController = navigation, startDestination = "chats") {
    composable("chats") {
      // Wide windows (tablets, unfolded foldables, landscape) show the conversation next to the list.
      val isWide = LocalConfiguration.current.screenWidthDp >= TWO_PANE_MIN_WIDTH_DP
      var maybeSelected by rememberSaveable { mutableStateOf<String?>(null) }
      LaunchedEffect(isWide) {
        // Folded or rotated to narrow: the open conversation stays open, as a screen of its own.
        val selected = maybeSelected
        if (!isWide && selected != null) {
          maybeSelected = null
          navigation.navigate("conversation/$selected")
        }
      }
      val list = @Composable {
        ChatListScreen(
          viewModel = viewModel,
          accountUserId = account.userId,
          accountName = account.displayName,
          onOpen = { if (isWide) maybeSelected = it else navigation.navigate("conversation/$it") },
          onSearch = { navigation.navigate("search") },
          onSettings = { navigation.navigate("settings") },
          onNavigate = { route -> navigation.navigate(route) },
          maybeSelectedId = maybeSelected.takeIf { isWide },
        )
      }
      if (!isWide) {
        list()
      } else {
        Row(Modifier.fillMaxSize()) {
          Box(Modifier.width(LIST_PANE_WIDTH_DP.dp).fillMaxHeight()) { list() }
          VerticalDivider()
          Box(Modifier.weight(1f).fillMaxHeight()) {
            when (val selected = maybeSelected) {
              null -> NoConversationSelected()
              else -> key(selected) {
                ConversationScreen(
                  viewModel = viewModel,
                  conversationId = selected,
                  onOpenThread = { threadId -> navigation.navigate("thread/$selected/$threadId") },
                  onOpenFiles = { navigation.navigate("files/$selected") },
                  onBack = { maybeSelected = null },
                  onOpenPermalink = openPermalink,
                  onOpenConversation = { maybeSelected = it },
                )
              }
            }
          }
        }
      }
    }
    composable(
      "conversation/{id}?focus={focus}",
      arguments = listOf(navArgument("id") { type = NavType.StringType }, focusArgument()),
    ) { entry ->
      val conversationId = entry.arguments?.getString("id").orEmpty()
      ConversationScreen(
        viewModel = viewModel,
        conversationId = conversationId,
        onOpenThread = { threadId -> navigation.navigate("thread/$conversationId/$threadId") },
        onOpenFiles = { navigation.navigate("files/$conversationId") },
        onBack = { navigation.popBackStack() },
        maybeFocusMessage = entry.arguments?.getString("focus"),
        onOpenPermalink = openPermalink,
        onOpenConversation = { navigation.navigate("conversation/$it") },
      )
    }
    composable(
      "thread/{conversation}/{thread}?focus={focus}",
      arguments = listOf(
        navArgument("conversation") { type = NavType.StringType },
        navArgument("thread") { type = NavType.StringType },
        focusArgument(),
      ),
    ) { entry ->
      ThreadScreen(
        viewModel = viewModel,
        conversationId = entry.arguments?.getString("conversation").orEmpty(),
        threadId = entry.arguments?.getString("thread").orEmpty(),
        onBack = { navigation.popBackStack() },
        maybeFocusMessage = entry.arguments?.getString("focus"),
        onOpenPermalink = openPermalink,
      )
    }
    composable("search") {
      SearchScreen(
        viewModel = viewModel,
        onOpen = { navigation.navigate("conversation/$it") },
        onBack = { navigation.popBackStack() },
      )
    }
    composable("threads") {
      ThreadsScreen(
        viewModel = viewModel,
        onOpenThread = { conversation, thread -> navigation.navigate("thread/$conversation/$thread") },
        onBack = { navigation.popBackStack() },
      )
    }
    composable("scheduled") {
      ScheduledScreen(viewModel = viewModel, onOpen = { navigation.navigate("conversation/$it") }, onBack = { navigation.popBackStack() })
    }
    composable("drafts") {
      DraftsAndSentScreen(
        viewModel = viewModel,
        onOpenDraft = { draft ->
          OpenedDraft.current.value = OpenedDraft.Target(draft.conversation, draft.maybeThread)
          navigation.navigate(
            draft.maybeThread?.let { thread -> "thread/${draft.conversation}/$thread" } ?: "conversation/${draft.conversation}",
          )
        },
        onOpenSent = { item ->
          navigation.navigate(
            item.maybeThread?.let { thread -> "thread/${item.conversation}/$thread?focus=${item.message}" }
              ?: "conversation/${item.conversation}?focus=${item.message}",
          )
        },
        onBack = { navigation.popBackStack() },
      )
    }
    composable("saved") {
      SavedScreen(viewModel = viewModel, onOpen = { navigation.navigate("conversation/$it") }, onBack = { navigation.popBackStack() })
    }
    composable(
      "browse?people={people}",
      arguments = listOf(navArgument("people") { type = NavType.BoolType; defaultValue = false }),
    ) { entry ->
      BrowseScreen(
        viewModel = viewModel,
        onOpen = { navigation.navigate("conversation/$it") },
        onBack = { navigation.popBackStack() },
        isPeopleFirst = entry.arguments?.getBoolean("people") == true,
      )
    }
    composable("meetings") {
      MeetingsScreen(viewModel = viewModel, onBack = { navigation.popBackStack() })
    }
    composable("space/{id}", arguments = listOf(navArgument("id") { type = NavType.StringType })) { entry ->
      SpaceViewsScreen(
        viewModel = viewModel,
        initialSpaceId = entry.arguments?.getString("id").orEmpty(),
        onOpen = { navigation.navigate("conversation/$it") },
        onBack = { navigation.popBackStack() },
      )
    }
    composable("files/{id}", arguments = listOf(navArgument("id") { type = NavType.StringType })) { entry ->
      FilesScreen(
        viewModel = viewModel,
        conversationId = entry.arguments?.getString("id").orEmpty(),
        onBack = { navigation.popBackStack() },
      )
    }
    composable("share") {
      val content = maybeShare
      if (content == null) {
        LaunchedEffect(Unit) { navigation.popBackStack() }
      } else {
        SharePickerScreen(
          viewModel = viewModel,
          state = content,
          onPick = { share, conversation ->
            PendingShare.current.value = PendingShare.Target(conversation, share)
            viewModel.handOverShare(share)
            navigation.navigate("conversation/$conversation") { popUpTo("chats") }
          },
          onBack = {
            viewModel.discardShare()
            navigation.popBackStack()
          },
        )
      }
    }
    composable("settings") {
      SettingsScreen(
        viewModel = viewModel,
        account = account,
        onBack = { navigation.popBackStack() },
        onDnd = { navigation.navigate("dnd") },
        onDiagnostics = { navigation.navigate("notification_diagnostics") },
        onReportProblem = { navigation.navigate("report_problem") },
      )
    }
    // The crash reports notice's way to the privacy switch.
    composable("settings/security") {
      SettingsScreen(
        viewModel = viewModel,
        account = account,
        onBack = { navigation.popBackStack() },
        onDnd = { navigation.navigate("dnd") },
        onDiagnostics = { navigation.navigate("notification_diagnostics") },
        onReportProblem = { navigation.navigate("report_problem") },
        opensSecurity = true,
      )
    }
    composable("report_problem") {
      ReportProblemScreen(viewModel = viewModel, account = account, onBack = { navigation.popBackStack() })
    }
    composable("dnd") {
      DoNotDisturbScreen(viewModel = viewModel, account = account, onBack = { navigation.popBackStack() })
    }
    composable("notification_diagnostics") {
      NotificationDiagnosticsScreen(
        viewModel = viewModel,
        account = account,
        onBattery = { navigation.navigate("push_battery") },
        onDnd = { navigation.navigate("dnd") },
        onBack = { navigation.popBackStack() },
      )
    }
    composable("push_battery") {
      PushBatteryScreen(viewModel = viewModel, onBack = { navigation.popBackStack() })
    }
  }
}

/** The message to open a conversation or thread at (from a message link); absent otherwise. */
private fun focusArgument() = navArgument("focus") {
  type = NavType.StringType
  nullable = true
  defaultValue = null
}

/** From this window width on, the chat list and the open conversation share the screen. */
private const val TWO_PANE_MIN_WIDTH_DP = 720
private const val LIST_PANE_WIDTH_DP = 360

@Composable
private fun NoConversationSelected() {
  Box(Modifier.fillMaxSize().background(MaterialTheme.colorScheme.background), contentAlignment = Alignment.Center) {
    Text(
      stringResource(R.string.no_conversation_selected),
      style = MaterialTheme.typography.bodyLarge,
      color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
  }
}
