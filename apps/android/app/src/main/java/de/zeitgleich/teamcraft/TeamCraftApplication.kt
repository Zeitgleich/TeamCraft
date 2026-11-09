package de.zeitgleich.teamcraft

import de.zeitgleich.teamcraft.settings.SettingsSync
import de.zeitgleich.teamcraft.ui.ShareReceiver
import de.zeitgleich.teamcraft.ui.StartCacheStore
import de.zeitgleich.teamcraft.ui.media.MediaDraftStore
import de.zeitgleich.teamcraft.ui.MAX_IMPORT_BYTES
import android.app.Application
import android.content.Context
import android.util.Log
import java.io.File
import androidx.lifecycle.DefaultLifecycleObserver
import androidx.lifecycle.LifecycleOwner
import androidx.lifecycle.ProcessLifecycleOwner
import androidx.work.Configuration
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import de.zeitgleich.teamcraft.auth.AuthRepository
import de.zeitgleich.teamcraft.calls.CallController
import de.zeitgleich.teamcraft.crash.CrashReports
import de.zeitgleich.teamcraft.crash.CrashSdks
import de.zeitgleich.teamcraft.crash.Perf
import de.zeitgleich.teamcraft.core.ActivityReports
import de.zeitgleich.teamcraft.core.CoreRepository
import de.zeitgleich.teamcraft.core.NetworkTrust
import de.zeitgleich.teamcraft.core.OutboxWork
import de.zeitgleich.teamcraft.notifications.AvatarCache
import de.zeitgleich.teamcraft.ui.SpacePictureCache
import de.zeitgleich.teamcraft.notifications.JoinRequestNotifier
import de.zeitgleich.teamcraft.notifications.JoinRequestWatcher
import de.zeitgleich.teamcraft.notifications.MentionPrefs
import de.zeitgleich.teamcraft.notifications.MessageNotifier
import de.zeitgleich.teamcraft.notifications.NotificationContentPrefs
import de.zeitgleich.teamcraft.notifications.CarProjection
import de.zeitgleich.teamcraft.notifications.PlaceholderWatcher
import de.zeitgleich.teamcraft.notifications.PushHandler
import de.zeitgleich.teamcraft.notifications.PushStats
import de.zeitgleich.teamcraft.notifications.ReadWatcher
import de.zeitgleich.teamcraft.push.PushManager
import de.zeitgleich.teamcraft.search.SegmentPrefetch
import de.zeitgleich.teamcraft.server.ManagedConfig
import de.zeitgleich.teamcraft.server.ServerStore
import de.zeitgleich.teamcraft.ui.ConnectionGrace
import de.zeitgleich.teamcraft.ui.NetworkWatcher
import de.zeitgleich.teamcraft.ui.Pictures
import uniffi.teamcraft_ffi.ConnectionState
import uniffi.teamcraft_ffi.AccountState
import uniffi.teamcraft_ffi.setDeletedUserName

/** Process-wide objects, shared by the activity, receivers and services. */
class AppGraph(context: Context) {
  val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)

  /** Crash reports to the instance's backend, when it has them on (docs/crash-reports.md). */
  val crashReports = CrashReports(context, CrashSdks.platform())

  /** What other apps shared, until it has a conversation (issue #208). */
  val shares = ShareReceiver(File(context.noBackupFilesDir, "share-inbox"), MAX_IMPORT_BYTES, scope)
  val servers = ServerStore(context)
  val managedConfig = ManagedConfig(context)
  /** Profile pictures for screens and notifications; they belong to the signed-in account. `core` is only used when called. */
  val avatars = AvatarCache(context) { core.core() }
  /** Pictures of spaces on screen, kept by version (issue #284). */
  val spacePictures = SpacePictureCache(context) { core.core() }
  /** The chat list shown last, for the next start's first frame (e2ee.md §9.2). */
  val startCache = StartCacheStore(context.filesDir)
  val auth: AuthRepository = AuthRepository(context, servers, onSignedOut = {
    avatars.clear()
    spacePictures.clear()
    // On every end of a session, also an expired refresh token: what the account showed last and
    // an unsent media flow don't stay for whoever signs in next (e2ee.md §9.2).
    startCache.delete()
    MediaDraftStore.deleteAll(context)
  })
  // Decrypted pictures leave memory with the account they belong to.
  val core: CoreRepository = CoreRepository(context, auth, onAccountClosed = {
    Pictures.clear()
    avatars.clear()
    spacePictures.clear()
  })
  val notificationContent = NotificationContentPrefs(context)
  val carProjection = CarProjection(context)
  val notifier = MessageNotifier(context, notificationContent, avatars, isInCar = carProjection::isActive)
  val mentionPrefs = MentionPrefs(context)
  /** Settings that follow the account to the user's other devices (#945). */
  val settingsSync = SettingsSync(context, core, mentionPrefs, scope)
  val calls = CallController(context, core, scope)
  val pushStats = PushStats(context)
  val pushHandler = PushHandler(context, notifier, calls, pushStats)
  val network = NetworkWatcher(context) { core.networkChanged() }
  val push = PushManager(context, core, pushHandler, mentionPrefs, notificationContent, carProjection, managedConfig, network.isOnline, scope)
  val placeholderWatcher = PlaceholderWatcher(core, notifier, pushHandler, scope, endDueHolds = { push.endHolds() })
  val segmentPrefetch = SegmentPrefetch(context)
  val readWatcher = ReadWatcher(core, notifier, scope)
  /** Admins asking to join a channel the user owns: notified while the app runs (#799). */
  val joinRequestNotifier = JoinRequestNotifier(context, notificationContent)
  val joinRequestWatcher = JoinRequestWatcher(core, joinRequestNotifier, scope)
  /** The sync loop should run but couldn't be started; the banner says so (issue #1142). */
  val isSyncStartFailing = MutableStateFlow(false)
  /** The connection banner's grace period, shared by every screen (issue #700). */
  val connectionGrace = ConnectionGrace(scope, core.connectionState, network.isOnline, isSyncStartFailing)
}

class TeamCraftApplication : Application(), Configuration.Provider {
  lateinit var graph: AppGraph
    private set

  override fun onCreate() {
    super.onCreate()
    graph = AppGraph(this)
    // First, so a crash during the rest of the start is reported (the last known setting).
    graph.crashReports.install()
    // Performance data of the app's own operations, through the same SDK (issue #1521).
    Perf.install(CrashSdks.platform())
    graph.scope.launch {
      var maybePrevious: ConnectionState? = null
      graph.core.connectionState.collect { current ->
        Perf.connectionChanged(maybePrevious, current)
        maybePrevious = current
      }
    }
    // The signed-in server's current setting, after every sign-in (and at start, signed in).
    graph.scope.launch {
      graph.auth.isSignedIn.collect { isSignedIn -> if (isSignedIn) refreshCrashReports(graph) }
    }
    // DM titles, members and notifications name deleted accounts in the user's language (#1066).
    // Off the main thread: the first call loads the core's native library, which is not needed
    // for the first frame. The core sets it again before it opens (CoreRepository.core).
    val deletedName = getString(R.string.user_deleted_name)
    graph.scope.launch { setDeletedUserName(deletedName) }
    // The core waits for these before it opens (issue #157).
    NetworkTrust.startInstalling(this, graph.scope, graph.network::currentLinkProperties)
    // The first use starts WorkManager (its database), which needn't hold up the first frame.
    graph.scope.launch(Dispatchers.IO) { graph.segmentPrefetch.schedule() }
    graph.carProjection.start { graph.scope.launch { graph.push.endHolds(isForced = true) } }
    graph.readWatcher.start()
    graph.settingsSync.start()
    graph.placeholderWatcher.start()
    graph.joinRequestWatcher.start()
    graph.network.start()
    // Older versions staged uploads in the cache, never deleted; the core's staging folder replaced it.
    graph.scope.launch(Dispatchers.IO) { File(cacheDir, "outgoing").deleteRecursively() }
    ProcessLifecycleOwner.get().lifecycle.addObserver(ForegroundSync(this, graph))
    ProcessLifecycleOwner.get().lifecycle.addObserver(ActivityReports(graph.core))
  }

  /** WorkManager starts on first use instead of at process start (AndroidManifest.xml). */
  override val workManagerConfiguration: Configuration
    get() = Configuration.Builder().build()

  /**
   * Syncs live (gateway) while the app is visible; in the background the push service wakes it.
   * Offline-first (issue #700): screens show the core's local store at once, whatever the
   * connection does; coming back only restarts the sync, and the banner's grace period with it.
   * Stopping waits [STOP_DELAY_MS], so a quick look at the camera or another app keeps the
   * connection instead of tearing it down and reconnecting. What still has to reach the server
   * then (a send just before leaving, a file going up) is handed to [OutboxWork] (issue #158), also
   * right away in case the process is killed within the delay.
   */
  private class ForegroundSync(private val context: Context, private val graph: AppGraph) : DefaultLifecycleObserver {
    private var maybeStopping: Job? = null
    private var maybeStarting: Job? = null
    /** Start and stop one after the other: a stop already under way finishes before the start. */
    private val order = Mutex()

    override fun onStart(owner: LifecycleOwner) {
      maybeStopping?.cancel()
      maybeStopping = null
      graph.connectionGrace.appReturned()
      graph.scope.launch { refreshCrashReports(graph) }
      maybeStarting?.cancel()
      // An access token that ran out is renewed while the core opens, so the connection doesn't
      // start with waiting for it.
      graph.scope.launch(Dispatchers.IO) { runCatching { graph.auth.accessToken() } }
      maybeStarting = graph.scope.launch { keepStarting() }
    }

    override fun onStop(owner: LifecycleOwner) {
      maybeStarting?.cancel()
      graph.isSyncStartFailing.value = false
      maybeStopping?.cancel()
      maybeStopping = graph.scope.launch {
        if (graph.auth.isSignedIn.value) scheduleOutbox()
        delay(STOP_DELAY_MS)
        order.withLock {
          // A run of OutboxWork under way keeps the sync going until it is done; the core stops it then.
          if (graph.auth.isSignedIn.value) runCatching { graph.core.core().stop() }
        }
      }
    }

    /** Hands what is still pending to WorkManager; one run sends it all, now or once a network is back. */
    private suspend fun scheduleOutbox() {
      val isPending = runCatching { !graph.core.core().pendingWork().isEmpty }.getOrDefault(false)
      if (isPending) OutboxWork.enqueue(context)
    }

    /**
     * Starts the sync and tries again while the app stays in the foreground: a failure must neither
     * vanish nor take a restart to get out of (issue #1142).
     */
    private suspend fun keepStarting() {
      var waitMs = START_RETRY_MIN_MS
      while (graph.auth.isSignedIn.value) {
        val maybeFailure = order.withLock { startSync() }
        graph.isSyncStartFailing.value = maybeFailure != null
        if (maybeFailure == null) return
        Log.w(TAG, "sync not started, next try in $waitMs ms: ${maybeFailure.javaClass.simpleName}")
        delay(waitMs)
        waitMs = (waitMs * 2).coerceAtMost(START_RETRY_MAX_MS)
      }
      graph.isSyncStartFailing.value = false
    }

    /** What kept the sync from starting, if anything. */
    private suspend fun startSync(): Throwable? =
      runCatching {
        val core = graph.core.core()
        if (core.accountState() is AccountState.Ready) {
          core.start()
          // Still running (back within the delay) or just started: check the connection now
          // instead of after a backoff that ran while the app was away.
          graph.core.networkChanged()
          graph.calls.start()
          graph.push.refresh()
        }
      }.exceptionOrNull().also { if (it is CancellationException) throw it }
  }

  companion object {
    private const val TAG = "TeamCraft"
    private const val START_RETRY_MIN_MS = 2_000L
    private const val START_RETRY_MAX_MS = 60_000L

    /** How long the sync keeps running after the app left the foreground. */
    private const val STOP_DELAY_MS = 10_000L

    fun graph(context: Context): AppGraph = (context.applicationContext as TeamCraftApplication).graph

    /** The signed-in server's crash reports setting; nothing while signed out. */
    private suspend fun refreshCrashReports(graph: AppGraph) {
      if (!graph.auth.isSignedIn.value) return
      val server = withContext(Dispatchers.IO) { graph.servers.active() } ?: return
      graph.crashReports.refresh(server)
    }
  }
}
