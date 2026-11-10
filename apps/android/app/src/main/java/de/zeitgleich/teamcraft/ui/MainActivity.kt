package de.zeitgleich.teamcraft.ui

import de.zeitgleich.teamcraft.crash.Perf
import de.zeitgleich.teamcraft.crash.StartVisibility
import android.content.Intent
import android.widget.Toast
import android.content.pm.ActivityInfo
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
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.fragment.app.FragmentActivity
import java.time.Duration
import java.time.Instant
import kotlinx.coroutines.launch
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.TeamCraftApplication
import de.zeitgleich.teamcraft.notifications.LaunchToken
import de.zeitgleich.teamcraft.security.AppLockPrefs
import de.zeitgleich.teamcraft.security.AppLockState
import de.zeitgleich.teamcraft.security.ScreenPrefs.Companion.prefs
import de.zeitgleich.teamcraft.security.promptUnlock
import de.zeitgleich.teamcraft.ui.screens.LoadingMarkOverlay
import de.zeitgleich.teamcraft.ui.screens.LockScreen
import de.zeitgleich.teamcraft.ui.screens.ShareWaitingNotice
import de.zeitgleich.teamcraft.ui.screens.SplashHandoff
import de.zeitgleich.teamcraft.ui.theme.TeamCraftTheme
import uniffi.teamcraft_ffi.AppActivity
import uniffi.teamcraft_ffi.setDeletedUserName

/** The system splash's assembly ends here (scripts/brand/mark_parts.py); the mark rests from then. */
private const val SPLASH_END_MS = 1040L
/** The smallest width from which a device counts as a tablet (Android's own sw600dp). */
private const val TABLET_MIN_WIDTH_DP = 600
/** The mark's share of the splash icon's side (88 of 288 dp, scripts/brand/mark_parts.py). */
private const val SPLASH_MARK_SHARE = 88f / 288f

/** Whether the share of the activity's intent is in the inbox (or was never there to take). */
private const val KEY_SHARE_TAKEN = "share_taken"
private const val KEY_OPEN_CONVERSATION = "open_conversation"
private const val KEY_APP_LINK = "app_link"

/** A [FragmentActivity] because the app lock's system prompt needs one. */
class MainActivity : FragmentActivity() {
  private val viewModel: MainViewModel by viewModels()
  /** A conversation to open, from a notification tap. */
  private val openConversation = mutableStateOf<String?>(null)
  /** The share of the intent this activity started with; the system hands that one back after a kill. */
  private var startShare: SharedContent? = null
  /** A link the system handed over, until it was acted on (docs/app-links.md). */
  private val appLink = mutableStateOf<String?>(null)
  private val isLocked = mutableStateOf(false)
  private val isUnlockRefused = mutableStateOf(false)
  /** Where the system splash left the TeamCraft mark, once it handed over (Android 12+). */
  private val splashHandoff = mutableStateOf<SplashHandoff?>(null)

  private val loginLauncher = registerForActivityResult(ActivityResultContracts.StartActivityForResult()) { result ->
    viewModel.completeLogin(result.data)
  }

  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    // Performance data: the signed-in start until the chat list shows the core's rows (#1521).
    // The first activity of a process starts cold, from the process start; later ones warm, and so
    // does a first one long after the process start (a push started the process, #1641).
    val createdAtMs = System.currentTimeMillis()
    val processStartedAtMs = processStartedAtMs()
    val isCold = !hasStartedBefore && StartVisibility.isColdStart(processStartedAtMs, createdAtMs)
    hasStartedBefore = true
    liveActivities++
    val startedAtMs = if (isCold) processStartedAtMs else createdAtMs
    // The entry path, the copies of the activity (each re-reads everything, #1375), and later the
    // first frame and whether it showed the start cache (#1431, #1428).
    Perf.beginStart(startedAtMs, createdAtMs, isCold, entryOf(intent), liveActivities)
    reportFirstFrame(startedAtMs)
    // A language change recreates the activity: the core names deleted accounts in the new one.
    // Off the main thread, like at start (TeamCraftApplication).
    val deletedName = getString(R.string.user_deleted_name)
    TeamCraftApplication.graph(this).scope.launch { setDeletedUserName(deletedName) }
    enableEdgeToEdge()
    // The main screen's bottom navigation bar reaches down behind the system's: no translucent
    // scrim over it with three-button navigation.
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) window.isNavigationBarContrastEnforced = false
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) handOverFromSplash()
    // Upright from the first frame while setting up (a recreated activity may be past that).
    keepUprightWhileSettingUp(isSettingUp = viewModel.appState.value !is AppState.Ready)
    applyScreenPrivacy()
    // Locked at a cold start; a rotation or a recreated activity keeps the unlocked state.
    isLocked.value = AppLockPrefs.prefs(this).isEnabled && !isUnlockedInProcess
    if (!isLocked.value) isUnlockedInProcess = true
    // Not again when the activity is recreated: a kept intent was acted on. What wasn't acted on yet
    // comes from the saved state.
    openConversation.value = when (savedInstanceState) {
      null -> conversationToOpen(intent)
      else -> savedInstanceState.getString(KEY_OPEN_CONVERSATION)
    }
    // Not again when the activity is recreated with an intent already taken, nor when the system
    // starts it from the recent apps with the old share's intent; an unfinished copy starts over.
    val isShareTaken = savedInstanceState?.getBoolean(KEY_SHARE_TAKEN) == true ||
      intent.flags and Intent.FLAG_ACTIVITY_LAUNCHED_FROM_HISTORY != 0
    if (!isShareTaken) {
      startShare = sharedContent(intent, packageName)
      startShare?.let { viewModel.receiveShare(it, isRedelivery = savedInstanceState != null) }
    }
    // A fresh start without a share (launcher, notification, link) never brings back the share
    // screen of a task the user closed; a restored activity keeps what waited.
    viewModel.onActivityCreated(isFreshStartWithoutShare = savedInstanceState == null && startShare == null)
    // Not again when the activity is recreated with the same intent (a rotation): it was acted on.
    appLink.value = when (savedInstanceState) {
      null -> appLinkOf(intent)
      else -> savedInstanceState.getString(KEY_APP_LINK)
    }
    setContent {
      // The server's brand colors the whole app (branding.md §5), not only the sign-in screen.
      val maybeBrand by viewModel.loginBrand.collectAsState()
      TeamCraftTheme(maybeBrand = maybeBrand) {
        val state by viewModel.appState.collectAsState()
        val isSettingUp = state !is AppState.Ready
        val maybeShare by viewModel.share.collectAsState()
        val hasShareFailed by viewModel.hasShareFailed.collectAsState()
        LaunchedEffect(hasShareFailed) {
          if (hasShareFailed) {
            Toast.makeText(this@MainActivity, R.string.share_failed, Toast.LENGTH_LONG).show()
            viewModel.shareFailureShown()
          }
        }
        val hasShareGone by viewModel.hasShareGoneForAccount.collectAsState()
        LaunchedEffect(hasShareGone) {
          if (hasShareGone) {
            Toast.makeText(this@MainActivity, R.string.share_other_account, Toast.LENGTH_LONG).show()
            viewModel.shareGoneForAccountShown()
          }
        }
        LaunchedEffect(isSettingUp) { keepUprightWhileSettingUp(isSettingUp) }
        // The automatic sign-in retry (issue #1063) opens the browser like the user's own sign-in.
        LaunchedEffect(Unit) { viewModel.loginRetryIntents.collect { intent -> loginLauncher.launch(intent) } }
        Box(Modifier.fillMaxSize()) {
          if (isLocked.value) {
            LockScreen(onUnlock = ::unlock, isUnlockRefused = isUnlockRefused.value)
            maybeShare?.let { ShareWaitingNotice(onDiscard = viewModel::discardShare, Modifier.align(Alignment.TopCenter)) }
          } else {
            AppRoot(
              viewModel = viewModel,
              maybeConversationToOpen = openConversation.value,
              onConversationOpened = { openConversation.value = null },
              maybeShare = maybeShare,
              maybeAppLink = appLink.value,
              onAppLinkHandled = { appLink.value = null },
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

  /**
   * Phones stay upright while the app sets up (loading screen, server entry, sign-in, keys): the
   * screens are built for portrait, and a turn mid-way would restart the loading animation.
   * Tablets and the app itself turn freely.
   */
  private fun keepUprightWhileSettingUp(isSettingUp: Boolean) {
    val isPhone = resources.configuration.smallestScreenWidthDp < TABLET_MIN_WIDTH_DP
    requestedOrientation =
      if (isSettingUp && isPhone) ActivityInfo.SCREEN_ORIENTATION_PORTRAIT else ActivityInfo.SCREEN_ORIENTATION_UNSPECIFIED
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

  /** Every touch and key press counts for presence; the core reports it at most every 30 s (#472). */
  override fun onUserInteraction() {
    super.onUserInteraction()
    TeamCraftApplication.graph(this).core.reportActivity(AppActivity.INTERACTION)
  }

  override fun onSaveInstanceState(outState: Bundle) {
    super.onSaveInstanceState(outState)
    // Only for the intent the system hands back after a kill: the one the activity started with.
    outState.putBoolean(KEY_SHARE_TAKEN, startShare?.let(viewModel::isCopyingShare) != true)
    outState.putString(KEY_OPEN_CONVERSATION, openConversation.value)
    outState.putString(KEY_APP_LINK, appLink.value)
  }

  override fun onDestroy() {
    liveActivities--
    super.onDestroy()
  }

  /** The first frame drawn: a step of the start, and whether it showed the start cache's chat list. */
  private fun reportFirstFrame(startedAtMs: Long) {
    val decor = window.decorView
    val listener = object : android.view.ViewTreeObserver.OnDrawListener {
      private var isDone = false

      override fun onDraw() {
        if (isDone) return
        isDone = true
        Perf.firstFrame(startedAtMs, isCachedChatList = viewModel.chatList.value.maybeConversations != null)
        // Not from inside the draw pass.
        decor.post { decor.viewTreeObserver.removeOnDrawListener(this) }
      }
    }
    decor.viewTreeObserver.addOnDrawListener(listener)
  }

  override fun onNewIntent(intent: Intent) {
    super.onNewIntent(intent)
    // For a configuration change. After a kill the system hands back the intent the activity started
    // with, so a copy of this share cut short by a kill is lost (docs/robustness-audit.md).
    setIntent(intent)
    conversationToOpen(intent)?.let { openConversation.value = it }
    sharedContent(intent, packageName)?.let(viewModel::receiveShare)
    appLinkOf(intent)?.let { appLink.value = it }
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
    promptUnlock(
      this,
      getString(R.string.app_lock_prompt),
      onUnlocked = {
        isUnlockedInProcess = true
        isUnlockRefused.value = false
        isLocked.value = false
      },
      onRefused = { isUnlockRefused.value = true },
    )
  }

  /** The conversation of one of the app's own notifications, or nothing (other apps' intents). */
  private fun conversationToOpen(intent: Intent): String? {
    if (!LaunchToken.isValid(this, intent.getStringExtra(EXTRA_LAUNCH_TOKEN))) return null
    return intent.getStringExtra(EXTRA_CONVERSATION_ID)?.takeIf(LaunchToken::isConversationId)
  }

  companion object {
    /** Whether an activity of this process was created before: the next start is warm. */
    private var hasStartedBefore = false

    /** Copies of this activity alive in the process (one is right, #1375). */
    private var liveActivities = 0

    const val EXTRA_CONVERSATION_ID = "conversation_id"
    const val EXTRA_LAUNCH_TOKEN = "launch_token"

    /** Per process, so the lock outlives the activity (recreated in the background, rotated). */
    private val lockState = AppLockState(SystemClock::elapsedRealtime)
    private var isUnlockedInProcess = false
  }
}

/** When this process started, Unix milliseconds (the system's uptime clock, moved to wall time). */
private fun processStartedAtMs(): Long =
  System.currentTimeMillis() - (android.os.SystemClock.uptimeMillis() - android.os.Process.getStartUptimeMillis())

/** How the app was opened, for the start's performance data. */
private fun entryOf(intent: Intent?): String = when {
  intent == null -> "launcher"
  intent.hasExtra(MainActivity.EXTRA_CONVERSATION_ID) -> "notification"
  intent.action == Intent.ACTION_SEND || intent.action == Intent.ACTION_SEND_MULTIPLE -> "share"
  intent.action == Intent.ACTION_VIEW && intent.data != null -> "link"
  else -> "launcher"
}
