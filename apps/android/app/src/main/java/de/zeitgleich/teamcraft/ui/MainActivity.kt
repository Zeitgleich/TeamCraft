package de.zeitgleich.teamcraft.ui

import android.Manifest
import android.content.Intent
import android.os.Build
import android.os.Bundle
import android.view.WindowManager.LayoutParams.FLAG_SECURE
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.activity.viewModels
import androidx.compose.runtime.mutableStateOf
import de.zeitgleich.teamcraft.notifications.LaunchToken
import de.zeitgleich.teamcraft.security.ScreenPrefs.Companion.prefs
import de.zeitgleich.teamcraft.ui.theme.TeamCraftTheme

class MainActivity : ComponentActivity() {
  private val viewModel: MainViewModel by viewModels()
  /** A conversation to open, from a notification tap. */
  private val openConversation = mutableStateOf<String?>(null)

  private val loginLauncher = registerForActivityResult(ActivityResultContracts.StartActivityForResult()) { result ->
    viewModel.completeLogin(result.data)
  }

  private val notificationPermission = registerForActivityResult(ActivityResultContracts.RequestPermission()) {}

  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    enableEdgeToEdge()
    if (prefs(this).isSecureScreen) window.setFlags(FLAG_SECURE, FLAG_SECURE)
    openConversation.value = conversationToOpen(intent)
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
      notificationPermission.launch(Manifest.permission.POST_NOTIFICATIONS)
    }
    setContent {
      TeamCraftTheme {
        AppRoot(
          viewModel = viewModel,
          maybeConversationToOpen = openConversation.value,
          onConversationOpened = { openConversation.value = null },
          onLogin = { intent -> loginLauncher.launch(intent) },
        )
      }
    }
  }

  override fun onNewIntent(intent: Intent) {
    super.onNewIntent(intent)
    conversationToOpen(intent)?.let { openConversation.value = it }
  }

  /** The conversation of one of the app's own notifications, or nothing (other apps' intents). */
  private fun conversationToOpen(intent: Intent): String? {
    if (!LaunchToken.isValid(this, intent.getStringExtra(EXTRA_LAUNCH_TOKEN))) return null
    return intent.getStringExtra(EXTRA_CONVERSATION_ID)?.takeIf(LaunchToken::isConversationId)
  }

  companion object {
    const val EXTRA_CONVERSATION_ID = "conversation_id"
    const val EXTRA_LAUNCH_TOKEN = "launch_token"
  }
}
