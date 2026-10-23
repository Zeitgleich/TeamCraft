package de.zeitgleich.teamcraft.ui

import android.Manifest
import android.content.Intent
import android.os.Build
import android.os.Bundle
import android.os.SystemClock
import android.view.WindowManager.LayoutParams.FLAG_SECURE
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.activity.viewModels
import androidx.compose.runtime.mutableStateOf
import androidx.fragment.app.FragmentActivity
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.notifications.LaunchToken
import de.zeitgleich.teamcraft.security.AppLockPrefs
import de.zeitgleich.teamcraft.security.AppLockState
import de.zeitgleich.teamcraft.security.ScreenPrefs.Companion.prefs
import de.zeitgleich.teamcraft.security.promptUnlock
import de.zeitgleich.teamcraft.ui.screens.LockScreen
import de.zeitgleich.teamcraft.ui.theme.TeamCraftTheme

/** A [FragmentActivity] because the app lock's system prompt needs one. */
class MainActivity : FragmentActivity() {
  private val viewModel: MainViewModel by viewModels()
  /** A conversation to open, from a notification tap. */
  private val openConversation = mutableStateOf<String?>(null)
  /** Text or files another app shared, until the user picked a conversation. */
  private val share = mutableStateOf<SharedContent?>(null)
  private val isLocked = mutableStateOf(false)

  private val loginLauncher = registerForActivityResult(ActivityResultContracts.StartActivityForResult()) { result ->
    viewModel.completeLogin(result.data)
  }

  private val notificationPermission = registerForActivityResult(ActivityResultContracts.RequestPermission()) {}

  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    enableEdgeToEdge()
    applyScreenPrivacy()
    // Locked at a cold start; a rotation or a recreated activity keeps the unlocked state.
    isLocked.value = AppLockPrefs.prefs(this).isEnabled && !isUnlockedInProcess
    if (!isLocked.value) isUnlockedInProcess = true
    openConversation.value = conversationToOpen(intent)
    share.value = sharedContent(intent, packageName)
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
      notificationPermission.launch(Manifest.permission.POST_NOTIFICATIONS)
    }
    setContent {
      TeamCraftTheme {
        if (isLocked.value) {
          LockScreen(onUnlock = ::unlock)
        } else {
          AppRoot(
            viewModel = viewModel,
            maybeConversationToOpen = openConversation.value,
            onConversationOpened = { openConversation.value = null },
            maybeShare = share.value,
            onShareHandled = { share.value = null },
            onLogin = { intent -> loginLauncher.launch(intent) },
          )
        }
      }
    }
  }

  override fun onStart() {
    super.onStart()
    val lock = AppLockPrefs.prefs(this)
    if (lockState.shouldLockOnForeground(lock.isEnabled, lock.lockAfter)) {
      isUnlockedInProcess = false
    }
    // Every instance follows the process-wide state: a second activity (a share or a notification
    // in a new task) that was created unlocked must not stay open after another one locked.
    if (lock.isEnabled && !isUnlockedInProcess) isLocked.value = true
    applyScreenPrivacy()
  }

  override fun onStop() {
    super.onStop()
    // Not while only a configuration change (rotation) stops the activity.
    if (!isChangingConfigurations) lockState.onBackground()
  }

  override fun onNewIntent(intent: Intent) {
    super.onNewIntent(intent)
    conversationToOpen(intent)?.let { openConversation.value = it }
    sharedContent(intent, packageName)?.let { share.value = it }
  }

  /**
   * Screenshots off when the user asked for it. With the app lock on, the recents thumbnail is
   * hidden too, so the locked app doesn't show the last conversation in the app switcher.
   */
  private fun applyScreenPrivacy() {
    val isLockOn = AppLockPrefs.prefs(this).isEnabled
    val isRecentsHidingAvailable = Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU
    if (isRecentsHidingAvailable) setRecentsScreenshotEnabled(!isLockOn)
    val isSecure = prefs(this).isSecureScreen || (isLockOn && !isRecentsHidingAvailable)
    if (isSecure) window.addFlags(FLAG_SECURE) else window.clearFlags(FLAG_SECURE)
  }

  private fun unlock() {
    promptUnlock(this, getString(R.string.app_lock_prompt)) {
      isUnlockedInProcess = true
      isLocked.value = false
    }
  }

  /** The conversation of one of the app's own notifications, or nothing (other apps' intents). */
  private fun conversationToOpen(intent: Intent): String? {
    if (!LaunchToken.isValid(this, intent.getStringExtra(EXTRA_LAUNCH_TOKEN))) return null
    return intent.getStringExtra(EXTRA_CONVERSATION_ID)?.takeIf(LaunchToken::isConversationId)
  }

  companion object {
    const val EXTRA_CONVERSATION_ID = "conversation_id"
    const val EXTRA_LAUNCH_TOKEN = "launch_token"

    /** Per process, so the lock outlives the activity (recreated in the background, rotated). */
    private val lockState = AppLockState(SystemClock::elapsedRealtime)
    private var isUnlockedInProcess = false
  }
}
