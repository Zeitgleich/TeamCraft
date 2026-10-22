package de.zeitgleich.teamcraft.push

import android.content.Context
import android.util.Log
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch
import de.zeitgleich.teamcraft.core.CoreRepository
import de.zeitgleich.teamcraft.notifications.MessageNotifier
import uniffi.teamcraft_ffi.PushService

/**
 * Chooses a push backend, registers its token with the server (with a fresh push key, e2ee.md §7)
 * and turns wake-ups into notifications.
 */
class PushManager(
  private val context: Context,
  private val core: CoreRepository,
  private val notifier: MessageNotifier,
  private val scope: CoroutineScope,
) {
  private val backends: List<PushBackend> = PushBackends.all()

  /** The backend in use, the first available in the flavor's order. */
  fun activeService(): PushService? = backends.firstOrNull { it.isAvailable(context) }?.service

  fun register() {
    backends.firstOrNull { it.isAvailable(context) }?.register(context)
  }

  fun onToken(service: PushService, token: String) {
    scope.launch {
      runCatching { core.core().registerPush(service, token) }
        .onFailure { Log.w(TAG, "push registration failed: ${it.javaClass.simpleName}") }
    }
  }

  /** Opens a wake-up and shows its notification. Never logs the payload or the text. */
  suspend fun onWakeUp(payload: ByteArray) {
    runCatching { core.core().notificationForPush(payload) }
      .onSuccess { notifier.show(it) }
      .onFailure { Log.w(TAG, "wake-up could not be opened: ${it.javaClass.simpleName}") }
  }

  private companion object {
    const val TAG = "TeamCraftPush"
  }
}
