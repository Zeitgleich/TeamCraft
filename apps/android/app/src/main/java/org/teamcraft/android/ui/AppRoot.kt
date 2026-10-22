package org.teamcraft.android.ui

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
import org.teamcraft.android.ui.screens.ChatListScreen
import org.teamcraft.android.ui.screens.ConversationScreen
import org.teamcraft.android.ui.screens.EnrollScreen
import org.teamcraft.android.ui.screens.LinkThisDeviceScreen
import org.teamcraft.android.ui.screens.LoadingScreen
import org.teamcraft.android.ui.screens.LoginScreen
import org.teamcraft.android.ui.screens.MessageScreen
import org.teamcraft.android.ui.screens.SearchScreen
import org.teamcraft.android.ui.screens.SettingsScreen
import org.teamcraft.android.ui.screens.ThreadScreen

@Composable
fun AppRoot(
  viewModel: MainViewModel,
  maybeConversationToOpen: String?,
  onConversationOpened: () -> Unit,
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
    )
    is AppState.LinkingThisDevice -> LinkThisDeviceScreen(
      code = current.code,
      maybeShortCode = current.maybeShortCode,
      onCancel = viewModel::cancelLinking,
    )
    is AppState.Failed -> MessageScreen(message = current.error, onRetry = viewModel::refresh)
    is AppState.Ready -> ReadyNavigation(viewModel, current, maybeConversationToOpen, onConversationOpened)
  }
}

@Composable
private fun ReadyNavigation(
  viewModel: MainViewModel,
  account: AppState.Ready,
  maybeConversationToOpen: String?,
  onConversationOpened: () -> Unit,
) {
  val navigation = rememberNavController()
  LaunchedEffect(maybeConversationToOpen) {
    maybeConversationToOpen?.let {
      navigation.navigate("conversation/$it")
      onConversationOpened()
    }
  }
  NavHost(navController = navigation, startDestination = "chats") {
    composable("chats") {
      ChatListScreen(
        viewModel = viewModel,
        onOpen = { navigation.navigate("conversation/$it") },
        onSearch = { navigation.navigate("search") },
        onSettings = { navigation.navigate("settings") },
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
    composable("settings") {
      SettingsScreen(viewModel = viewModel, account = account, onBack = { navigation.popBackStack() })
    }
  }
}
