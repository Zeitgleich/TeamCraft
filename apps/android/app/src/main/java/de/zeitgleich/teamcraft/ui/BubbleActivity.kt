package de.zeitgleich.teamcraft.ui

import android.content.Context
import android.content.Intent
import android.os.Bundle
import android.view.WindowManager.LayoutParams.FLAG_SECURE
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.viewModels
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.fragment.app.FragmentActivity
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.TeamCraftApplication
import de.zeitgleich.teamcraft.notifications.LaunchToken
import de.zeitgleich.teamcraft.security.AppLockPrefs
import de.zeitgleich.teamcraft.security.ScreenPrefs.Companion.prefs
import de.zeitgleich.teamcraft.ui.screens.ConversationScreen
import de.zeitgleich.teamcraft.ui.screens.MessageScreen
import de.zeitgleich.teamcraft.ui.screens.ThreadScreen
import de.zeitgleich.teamcraft.ui.theme.TeamCraftTheme

/**
 * The expanded conversation bubble of a direct message (issue #591): one conversation in a small
 * floating window, with the conversation screen of the app. A separate entry beside [MainActivity];
 * the notification tap, shares and links still go to the main activity.
 *
 * It shows a conversation only while the signed-in app can: with the app lock on, or in a state that
 * needs sign-in or any other set-up step, it sends the user to the main activity instead. The
 * notification carries no bubble in the app lock's case, so this covers a lock turned on later.
 */
class BubbleActivity : FragmentActivity() {
  private val viewModel: MainViewModel by viewModels()

  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    val maybeConversationId = conversationOf(intent)
    if (maybeConversationId == null) {
      finish()
      return
    }
    if (AppLockPrefs.prefs(this).isEnabled) {
      // The lock screen belongs to the main activity; the bubble would show chats behind it.
      openInApp(this, maybeConversationId)
      finish()
      return
    }
    if (prefs(this).isSecureScreen) window.addFlags(FLAG_SECURE)
    enableEdgeToEdge()
    // No share of its own, and it must not drop one that waits for the main activity.
    viewModel.onActivityCreated(isFreshStartWithoutShare = false)
    setContent {
      TeamCraftTheme {
        Surface(Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.background) {
          BubbleContent(viewModel, maybeConversationId, onClose = ::finish)
        }
      }
    }
  }

  /** The conversation of a bubble the app itself posted, or nothing (other apps' intents). */
  private fun conversationOf(intent: Intent): String? {
    if (!LaunchToken.isValid(this, intent.getStringExtra(MainActivity.EXTRA_LAUNCH_TOKEN))) return null
    return intent.getStringExtra(MainActivity.EXTRA_CONVERSATION_ID)?.takeIf(LaunchToken::isConversationId)
  }

  companion object {
    /** The conversation in the app's own window, for what a bubble does not show (files, other chats). */
    fun openInApp(context: Context, conversationId: String) {
      val intent = Intent(context, MainActivity::class.java)
        .putExtra(MainActivity.EXTRA_CONVERSATION_ID, conversationId)
        .putExtra(MainActivity.EXTRA_LAUNCH_TOKEN, LaunchToken.of(context))
        .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP)
      context.startActivity(intent)
    }
  }
}

@Composable
private fun BubbleContent(viewModel: MainViewModel, conversationId: String, onClose: () -> Unit) {
  val context = LocalContext.current
  val state by viewModel.appState.collectAsState()
  when (val current = state) {
    AppState.Starting -> Column(
      Modifier.fillMaxSize(),
      verticalArrangement = Arrangement.Center,
      horizontalAlignment = Alignment.CenterHorizontally,
    ) { CircularProgressIndicator() }
    is AppState.Failed -> MessageScreen(message = current.error, onRetry = viewModel::refresh)
    is AppState.Ready -> ReadyBubble(viewModel, conversationId, onClose, openInApp = { BubbleActivity.openInApp(context, it) })
    // Signing in, linking or recovering happens in the app, not in a small window.
    else -> Column(
      Modifier.fillMaxSize().padding(24.dp),
      verticalArrangement = Arrangement.spacedBy(16.dp, Alignment.CenterVertically),
      horizontalAlignment = Alignment.CenterHorizontally,
    ) {
      Text(stringResource(R.string.bubble_needs_app), style = MaterialTheme.typography.bodyLarge)
      Button(onClick = { BubbleActivity.openInApp(context, conversationId) }) { Text(stringResource(R.string.bubble_open_app)) }
    }
  }
}

@Composable
private fun ReadyBubble(viewModel: MainViewModel, conversationId: String, onClose: () -> Unit, openInApp: (String) -> Unit) {
  val context = LocalContext.current
  val changes by viewModel.changeCount.collectAsState()
  var maybeThread by rememberSaveable { mutableStateOf<String?>(null) }
  // Deleted or left while the bubble is up: the bubble goes with it. An empty list is not proof (the
  // read can fail), so only a list that lacks the conversation counts.
  LaunchedEffect(changes) {
    val all = viewModel.conversations()
    if (all.isNotEmpty() && all.none { it.id == conversationId }) {
      TeamCraftApplication.graph(context).notifier.dismiss(conversationId)
      onClose()
    }
  }
  val thread = maybeThread
  if (thread == null) {
    ConversationScreen(
      viewModel = viewModel,
      conversationId = conversationId,
      onOpenThread = { maybeThread = it },
      onOpenFiles = { openInApp(conversationId) },
      onBack = onClose,
      onOpenPermalink = { openInApp(conversationId) },
      onOpenConversation = openInApp,
    )
  } else {
    ThreadScreen(
      viewModel = viewModel,
      conversationId = conversationId,
      threadId = thread,
      onBack = { maybeThread = null },
      onOpenPermalink = { openInApp(conversationId) },
    )
  }
}
