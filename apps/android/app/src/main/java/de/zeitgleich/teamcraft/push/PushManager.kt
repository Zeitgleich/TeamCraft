package de.zeitgleich.teamcraft.push

import android.content.Context
import android.util.Log
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch
import de.zeitgleich.teamcraft.core.CoreRepository
import de.zeitgleich.teamcraft.notifications.MentionPrefs
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
  private val mentionPrefs: MentionPrefs,
  private val scope: CoroutineScope,
) {
  private val backends: List<PushBackend> = PushBackends.all()

  /** The backend in use, the first available in the flavor's order. */
  fun activeService(): PushService? = backends.firstOrNull { it.isAvailable(context) }?.service

  fun register() {
    backends.firstOrNull { it.isAvailable(context) }?.register(context)
  }

  /**
   * Sign-out: removes the registration on the server (and with it the push key on this device) and
   * with the push services. Best effort; a wake-up that still arrives can't be opened.
   */
  suspend fun unregister() {
    runCatching { core.core().unregisterPush() }
      .onFailure { Log.w(TAG, "push unregistration failed: ${it.javaClass.simpleName}") }
    backends.forEach { runCatching { it.unregister(context) } }
  }

  fun onToken(service: PushService, token: String) {
    scope.launch {
      runCatching { core.core().registerPush(service, token) }
        .onFailure { Log.w(TAG, "push registration failed: ${it.javaClass.simpleName}") }
    }
  }

  /** Opens a wake-up and shows its notification. Never logs the payload or the text. */
  suspend fun onWakeUp(payload: ByteArray) {
    runCatching {
      val core = core.core()
      // Keywords and the first name are matched on the device (the server can't read the text).
      core.setMentionSettings(mentionPrefs.settings.value)
      core.notificationForPush(payload)
    }
      .onSuccess { notifier.show(it) }
      .onFailure { Log.w(TAG, "wake-up could not be opened: ${it.javaClass.simpleName}") }
  }

  private companion object {
    const val TAG = "TeamCraftPush"
  }
}
