package de.zeitgleich.teamcraft.ui

import android.content.Intent
import android.os.Build
import android.os.Bundle
import android.os.SystemClock
import android.view.WindowManager.LayoutParams.FLAG_SECURE
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.activity.viewModels
import androidx.annotation.RequiresApi
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.fragment.app.FragmentActivity
import java.time.Duration
import java.time.Instant
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.notifications.LaunchToken
import de.zeitgleich.teamcraft.security.AppLockPrefs
import de.zeitgleich.teamcraft.security.AppLockState
import de.zeitgleich.teamcraft.security.ScreenPrefs.Companion.prefs
import de.zeitgleich.teamcraft.security.promptUnlock
import de.zeitgleich.teamcraft.ui.screens.LoadingMarkOverlay
import de.zeitgleich.teamcraft.ui.screens.LockScreen
import de.zeitgleich.teamcraft.ui.screens.SplashHandoff
import de.zeitgleich.teamcraft.ui.theme.TeamCraftTheme

/** The system splash's assembly ends here (scripts/brand/mark_parts.py); the mark rests from then. */
private const val SPLASH_END_MS = 1040L
/** The mark's share of the splash icon's side (88 of 288 dp, scripts/brand/mark_parts.py). */
private const val SPLASH_MARK_SHARE = 88f / 288f

/** A [FragmentActivity] because the app lock's system prompt needs one. */
class MainActivity : FragmentActivity() {
  private val viewModel: MainViewModel by viewModels()
  /** A conversation to open, from a notification tap. */
  private val openConversation = mutableStateOf<String?>(null)
  /** Text or files another app shared, until the user picked a conversation. */
  private val share = mutableStateOf<SharedContent?>(null)
  private val isLocked = mutableStateOf(false)
  /** Where the system splash left the TeamCraft mark, once it handed over (Android 12+). */
  private val splashHandoff = mutableStateOf<SplashHandoff?>(null)

  private val loginLauncher = registerForActivityResult(ActivityResultContracts.StartActivityForResult()) { result ->
    viewModel.completeLogin(result.data)
  }

  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    enableEdgeToEdge()
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) handOverFromSplash()
    applyScreenPrivacy()
    // Locked at a cold start; a rotation or a recreated activity keeps the unlocked state.
    isLocked.value = AppLockPrefs.prefs(this).isEnabled && !isUnlockedInProcess
    if (!isLocked.value) isUnlockedInProcess = true
    openConversation.value = conversationToOpen(intent)
    share.value = sharedContent(intent, packageName)
    setContent {
      // The server's brand colors the whole app (branding.md §5), not only the sign-in screen.
      val maybeBrand by viewModel.loginBrand.collectAsState()
      TeamCraftTheme(maybeBrand = maybeBrand) {
        val state by viewModel.appState.collectAsState()
        Box(Modifier.fillMaxSize()) {
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
          // The animated mark while the core starts, from where the system splash left it (#463).
          LoadingMarkOverlay(isLoading = state == AppState.Starting, maybeHandoff = splashHandoff.value)
        }
      }
    }
  }

  /**
   * The system splash plays the mark's assembly (drawable-v31/splash_mark.xml); the app's loading
   * screen takes it over at the splash's clock, place and size, and the splash goes once that is
   * drawn, so the hand-over is invisible.
   */
  @RequiresApi(Build.VERSION_CODES.S)
  private fun handOverFromSplash() {
    splashScreen.setOnExitAnimationListener { splash ->
      val icon = splash.iconView
      val elapsed = splash.iconAnimationStart?.let { Duration.between(it, Instant.now()).toMillis() } ?: SPLASH_END_MS
      if (icon != null) {
        val location = IntArray(2).also(icon::getLocationInWindow)
        splashHandoff.value = SplashHandoff(
          elapsedMs = elapsed.coerceIn(0L, SPLASH_END_MS),
          centre = Offset(location[0] + icon.width / 2f, location[1] + icon.height / 2f),
          markSizePx = icon.width * SPLASH_MARK_SHARE,
        )
      }
      // Two frames: the loading screen draws the taken-over mark before the splash goes.
      window.decorView.postOnAnimation { window.decorView.postOnAnimation { splash.remove() } }
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
