package de.zeitgleich.teamcraft

import android.app.Application
import android.content.Context
import java.io.File
import androidx.lifecycle.DefaultLifecycleObserver
import androidx.lifecycle.LifecycleOwner
import androidx.lifecycle.ProcessLifecycleOwner
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import de.zeitgleich.teamcraft.auth.AuthRepository
import de.zeitgleich.teamcraft.calls.CallController
import de.zeitgleich.teamcraft.core.ActivityReports
import de.zeitgleich.teamcraft.core.CoreRepository
import de.zeitgleich.teamcraft.core.NetworkTrust
import de.zeitgleich.teamcraft.notifications.AvatarCache
import de.zeitgleich.teamcraft.notifications.JoinRequestNotifier
import de.zeitgleich.teamcraft.notifications.JoinRequestWatcher
import de.zeitgleich.teamcraft.notifications.MentionPrefs
import de.zeitgleich.teamcraft.notifications.MessageNotifier
import de.zeitgleich.teamcraft.notifications.NotificationContentPrefs
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
import uniffi.teamcraft_ffi.AccountState

/** Process-wide objects, shared by the activity, receivers and services. */
class AppGraph(context: Context) {
  val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
  val servers = ServerStore(context)
  val managedConfig = ManagedConfig(context)
  // Profile pictures of the notifications belong to the signed-in account; `core` is only used when called.
  private val avatars = AvatarCache(context) { core.core() }
  val auth: AuthRepository = AuthRepository(context, servers, onSignedOut = avatars::clear)
  // Decrypted pictures leave memory with the account they belong to.
  val core: CoreRepository = CoreRepository(context, auth, onAccountClosed = {
    Pictures.clear()
    avatars.clear()
  })
  val notificationContent = NotificationContentPrefs(context)
  val notifier = MessageNotifier(context, notificationContent, avatars)
  val mentionPrefs = MentionPrefs(context)
  val calls = CallController(context, core, scope)
  val pushStats = PushStats(context)
  val pushHandler = PushHandler(context, notifier, calls, pushStats)
  val push = PushManager(context, core, pushHandler, mentionPrefs, notificationContent, scope)
  val segmentPrefetch = SegmentPrefetch(context)
  val readWatcher = ReadWatcher(core, notifier, scope)
  /** Admins asking to join a channel the user owns: notified while the app runs (#799). */
  val joinRequestNotifier = JoinRequestNotifier(context, notificationContent)
  val joinRequestWatcher = JoinRequestWatcher(core, joinRequestNotifier, scope)
  val network = NetworkWatcher(context) { core.networkChanged() }
  /** The connection banner's grace period, shared by every screen (issue #700). */
  val connectionGrace = ConnectionGrace(scope, core.connectionState, network.isOnline)
}

class TeamCraftApplication : Application() {
  lateinit var graph: AppGraph
    private set

  override fun onCreate() {
    super.onCreate()
    graph = AppGraph(this)
    // The core waits for these before it opens (issue #157).
    NetworkTrust.startInstalling(this, graph.scope)
    graph.segmentPrefetch.schedule()
    graph.readWatcher.start()
    graph.joinRequestWatcher.start()
    graph.network.start()
    // Older versions staged uploads in the cache, never deleted; the core's staging folder replaced it.
    graph.scope.launch(Dispatchers.IO) { File(cacheDir, "outgoing").deleteRecursively() }
    ProcessLifecycleOwner.get().lifecycle.addObserver(ForegroundSync(graph))
    ProcessLifecycleOwner.get().lifecycle.addObserver(ActivityReports(graph.core))
  }

  /**
   * Syncs live (gateway) while the app is visible; in the background the push service wakes it.
   * Offline-first (issue #700): screens show the core's local store at once, whatever the
   * connection does; coming back only restarts the sync, and the banner's grace period with it.
   * Stopping waits [STOP_DELAY_MS], so a quick look at the camera or another app keeps the
   * connection instead of tearing it down and reconnecting.
   */
  private class ForegroundSync(private val graph: AppGraph) : DefaultLifecycleObserver {
    private var maybeStopping: Job? = null
    /** Start and stop one after the other: a stop already under way finishes before the start. */
    private val order = Mutex()

    override fun onStart(owner: LifecycleOwner) {
      maybeStopping?.cancel()
      maybeStopping = null
      graph.connectionGrace.appReturned()
      graph.scope.launch {
        if (!graph.auth.isSignedIn.value) return@launch
        order.withLock { startSync() }
      }
    }

    override fun onStop(owner: LifecycleOwner) {
      maybeStopping?.cancel()
      maybeStopping = graph.scope.launch {
        delay(STOP_DELAY_MS)
        order.withLock {
          if (graph.auth.isSignedIn.value) runCatching { graph.core.core().stop() }
        }
      }
    }

    private suspend fun startSync() {
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
      }
    }
  }

  companion object {
    /** How long the sync keeps running after the app left the foreground. */
    private const val STOP_DELAY_MS = 10_000L

    fun graph(context: Context): AppGraph = (context.applicationContext as TeamCraftApplication).graph
  }
}
