package org.teamcraft.android.ui

import android.Manifest
import android.content.Intent
import android.os.Build
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.activity.viewModels
import androidx.compose.runtime.mutableStateOf
import org.teamcraft.android.ui.theme.TeamCraftTheme

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
    openConversation.value = intent.getStringExtra(EXTRA_CONVERSATION_ID)
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
    intent.getStringExtra(EXTRA_CONVERSATION_ID)?.let { openConversation.value = it }
  }

  companion object {
    const val EXTRA_CONVERSATION_ID = "conversation_id"
  }
}
