package de.zeitgleich.teamcraft

import android.app.Application
import android.content.Context
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
import de.zeitgleich.teamcraft.notifications.MessageNotifier
import de.zeitgleich.teamcraft.push.PushManager
import uniffi.teamcraft_ffi.AccountState

/** Process-wide objects, shared by the activity, receivers and services. */
class AppGraph(context: Context) {
  val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
  val auth = AuthRepository(context)
  val core = CoreRepository(context, auth)
  val notifier = MessageNotifier(context)
  val push = PushManager(context, core, notifier, scope)
  val calls = CallController(context, core, scope)
}

class TeamCraftApplication : Application() {
  lateinit var graph: AppGraph
    private set

  override fun onCreate() {
    super.onCreate()
    graph = AppGraph(this)
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
