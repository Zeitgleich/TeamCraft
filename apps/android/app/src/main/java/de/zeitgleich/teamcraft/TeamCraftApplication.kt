package de.zeitgleich.teamcraft

import android.app.Application
import android.content.Context
import java.io.File
import androidx.lifecycle.DefaultLifecycleObserver
import androidx.lifecycle.LifecycleOwner
import androidx.lifecycle.ProcessLifecycleOwner
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch
import de.zeitgleich.teamcraft.auth.AuthRepository
import de.zeitgleich.teamcraft.calls.CallController
import de.zeitgleich.teamcraft.core.CoreRepository
import de.zeitgleich.teamcraft.core.NetworkTrust
import de.zeitgleich.teamcraft.notifications.AvatarCache
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
  val network = NetworkWatcher(context) { core.networkChanged() }
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
    graph.network.start()
    // Older versions staged uploads in the cache, never deleted; the core's staging folder replaced it.
    graph.scope.launch(Dispatchers.IO) { File(cacheDir, "outgoing").deleteRecursively() }
    ProcessLifecycleOwner.get().lifecycle.addObserver(ForegroundSync(graph))
  }

  /**
   * Syncs live (gateway) while the app is visible; in the background the push service wakes it.
   */
  private class ForegroundSync(private val graph: AppGraph) : DefaultLifecycleObserver {
    override fun onStart(owner: LifecycleOwner) {
      graph.scope.launch {
        if (!graph.auth.isSignedIn.value) return@launch
        runCatching {
          val core = graph.core.core()
          if (core.accountState() is AccountState.Ready) {
            core.start()
            graph.calls.start()
            graph.push.refresh()
          }
        }
      }
    }

    override fun onStop(owner: LifecycleOwner) {
      graph.scope.launch {
        if (graph.auth.isSignedIn.value) runCatching { graph.core.core().stop() }
      }
    }
  }

  companion object {
    fun graph(context: Context): AppGraph = (context.applicationContext as TeamCraftApplication).graph
  }
}
