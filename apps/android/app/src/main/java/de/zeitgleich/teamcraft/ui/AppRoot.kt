package de.zeitgleich.teamcraft.ui

import android.content.Intent
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.rememberCoroutineScope
import androidx.navigation.NavType
import androidx.navigation.compose.NavHost
import androidx.navigation.compose.composable
import androidx.navigation.compose.rememberNavController
import androidx.navigation.navArgument
import kotlinx.coroutines.launch
import de.zeitgleich.teamcraft.ui.screens.BrowseScreen
import de.zeitgleich.teamcraft.ui.screens.ChatListScreen
import de.zeitgleich.teamcraft.ui.screens.FilesScreen
import de.zeitgleich.teamcraft.ui.screens.MeetingsScreen
import de.zeitgleich.teamcraft.ui.screens.SavedScreen
import de.zeitgleich.teamcraft.ui.screens.ThreadsScreen
import de.zeitgleich.teamcraft.ui.screens.ConversationScreen
import de.zeitgleich.teamcraft.ui.screens.DoNotDisturbScreen
import de.zeitgleich.teamcraft.ui.screens.NotificationDiagnosticsScreen
import de.zeitgleich.teamcraft.ui.screens.PushBatteryScreen
import de.zeitgleich.teamcraft.ui.screens.EnrollScreen
import de.zeitgleich.teamcraft.ui.screens.LinkThisDeviceScreen
import de.zeitgleich.teamcraft.ui.screens.LoadingScreen
import de.zeitgleich.teamcraft.ui.screens.LoginScreen
import de.zeitgleich.teamcraft.ui.screens.MessageScreen
import de.zeitgleich.teamcraft.ui.screens.OrgRecoveryScreen
import de.zeitgleich.teamcraft.ui.screens.SearchScreen
import de.zeitgleich.teamcraft.ui.screens.SettingsScreen
import de.zeitgleich.teamcraft.ui.screens.SharePickerScreen
import de.zeitgleich.teamcraft.ui.screens.ThreadScreen

@Composable
fun AppRoot(
  viewModel: MainViewModel,
  maybeConversationToOpen: String?,
  onConversationOpened: () -> Unit,
  maybeShare: SharedContent?,
  onShareHandled: () -> Unit,
  onLogin: (Intent) -> Unit,
) {
  val state by viewModel.appState.collectAsState()
  val isBusy by viewModel.isBusy.collectAsState()
  val scope = rememberCoroutineScope()
  when (val current = state) {
    AppState.Starting -> LoadingScreen()
    is AppState.SignedOut -> LoginScreen(
      maybeError = current.maybeError,
      isBusy = isBusy,
      onLogin = { scope.launch { runCatching { onLogin(viewModel.loginIntent()) } } },
    )
    is AppState.NeedsEnrollment -> EnrollScreen(
      hasExistingIdentity = current.hasExistingIdentity,
      maybeError = current.maybeError,
      isBusy = isBusy,
      onEnroll = viewModel::enroll,
      onLink = viewModel::linkThisDevice,
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
      code = current.code,
      maybeShortCode = current.maybeShortCode,
      onCancel = viewModel::cancelLinking,
    )
    is AppState.Failed -> MessageScreen(message = current.error, onRetry = viewModel::refresh)
    is AppState.Ready -> ReadyNavigation(viewModel, current, maybeConversationToOpen, onConversationOpened, maybeShare, onShareHandled)
  }
}

@Composable
private fun ReadyNavigation(
  viewModel: MainViewModel,
  account: AppState.Ready,
  maybeConversationToOpen: String?,
  onConversationOpened: () -> Unit,
  maybeShare: SharedContent?,
  onShareHandled: () -> Unit,
) {
  val navigation = rememberNavController()
  LaunchedEffect(maybeConversationToOpen) {
    maybeConversationToOpen?.let {
      navigation.navigate("conversation/$it")
      onConversationOpened()
    }
  }
  LaunchedEffect(maybeShare) {
    if (maybeShare != null) navigation.navigate("share") { launchSingleTop = true }
  }
  NavHost(navController = navigation, startDestination = "chats") {
    composable("chats") {
      ChatListScreen(
        viewModel = viewModel,
        onOpen = { navigation.navigate("conversation/$it") },
        onSearch = { navigation.navigate("search") },
        onSettings = { navigation.navigate("settings") },
        onNavigate = { route -> navigation.navigate(route) },
      )
    }
    composable(
      "conversation/{id}",
      arguments = listOf(navArgument("id") { type = NavType.StringType }),
    ) { entry ->
      val conversationId = entry.arguments?.getString("id").orEmpty()
      ConversationScreen(
        viewModel = viewModel,
        conversationId = conversationId,
        onOpenThread = { threadId -> navigation.navigate("thread/$conversationId/$threadId") },
        onOpenFiles = { navigation.navigate("files/$conversationId") },
        onBack = { navigation.popBackStack() },
      )
    }
    composable(
      "thread/{conversation}/{thread}",
      arguments = listOf(
        navArgument("conversation") { type = NavType.StringType },
        navArgument("thread") { type = NavType.StringType },
      ),
    ) { entry ->
      ThreadScreen(
        viewModel = viewModel,
        conversationId = entry.arguments?.getString("conversation").orEmpty(),
        threadId = entry.arguments?.getString("thread").orEmpty(),
        onBack = { navigation.popBackStack() },
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
    composable("saved") {
      SavedScreen(viewModel = viewModel, onOpen = { navigation.navigate("conversation/$it") }, onBack = { navigation.popBackStack() })
    }
    composable("browse") {
      BrowseScreen(viewModel = viewModel, onOpen = { navigation.navigate("conversation/$it") }, onBack = { navigation.popBackStack() })
    }
    composable("meetings") {
      MeetingsScreen(viewModel = viewModel, onBack = { navigation.popBackStack() })
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
          content = content,
          onPick = { conversation ->
            PendingShare.current.value = PendingShare.Target(conversation, content)
            onShareHandled()
            navigation.navigate("conversation/$conversation") { popUpTo("chats") }
          },
          onBack = {
            onShareHandled()
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
      )
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
