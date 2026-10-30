package de.zeitgleich.teamcraft.push

import android.content.Context
import android.util.Log
import java.util.TimeZone
import java.util.concurrent.ConcurrentHashMap
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Deferred
import kotlinx.coroutines.async
import kotlinx.coroutines.launch
import de.zeitgleich.teamcraft.core.CoreRepository
import de.zeitgleich.teamcraft.notifications.MentionPrefs
import de.zeitgleich.teamcraft.notifications.NotificationContentPrefs
import de.zeitgleich.teamcraft.notifications.PushHandler
import de.zeitgleich.teamcraft.notifications.PushRetry
import uniffi.teamcraft_ffi.PushNotification
import uniffi.teamcraft_ffi.PushService
import uniffi.teamcraft_ffi.TeamCraftException

/**
 * Chooses a push backend, registers its token with the server (the push key stays the same while
 * the service does, e2ee.md §7) and turns wake-ups into notifications.
 */
class PushManager(
  private val context: Context,
  private val core: CoreRepository,
  private val handler: PushHandler,
  private val mentionPrefs: MentionPrefs,
  private val contentPrefs: NotificationContentPrefs,
  private val scope: CoroutineScope,
) {
  private val backends: List<PushBackend> = PushBackends.all()

  /** Wake-ups being opened, by payload: a hand-over to [WakeUpWork] waits for the same run. */
  private val running = ConcurrentHashMap<String, Deferred<Unit>>()

  /**
   * A token whose registration failed (server unreachable): retried in the background once a network
   * is there ([PushRegistrationRetry]) and on the next foreground.
   */
  @Volatile private var maybePendingToken: Pair<PushService, String>? = null

  /** Set while (and after) signing out: wake-ups arriving then show nothing. */
  @Volatile private var isSigningOut = false

  /** Drops every wake-up from now on until the next [register] (sign-out). */
  fun beginSignOut() {
    isSigningOut = true
  }

  /** The backend in use, the first available in the flavor's order. */
  fun activeService(): PushService? = backends.firstOrNull { it.isAvailable(context) }?.service

  fun register() {
    isSigningOut = false
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
    scope.launch { registerToken(service, token) }
  }

  /**
   * The app came to the foreground: retries a registration that failed, otherwise seals the
   * registered token again when that is due (it is sealed for one server and expires). Without
   * this, a token rotated while the server was unreachable, or an expired seal, left push broken
   * until the next sign-in.
   */
  suspend fun refresh() {
    val pending = maybePendingToken
    if (pending != null) {
      registerToken(pending.first, pending.second)
      return
    }
    runCatching { core.core().refreshPush() }
      .onFailure { Log.w(TAG, "push refresh failed: ${it.javaClass.simpleName}") }
  }

  /**
   * A background attempt of [PushRegistrationRetry]: the pending token if this process still has
   * it, otherwise the push service is asked for its token again (it arrives at [onToken]).
   */
  suspend fun retryRegistration() {
    if (isSigningOut) return
    val pending = maybePendingToken
    if (pending != null) {
      registerToken(pending.first, pending.second)
    } else {
      backends.firstOrNull { it.isAvailable(context) }?.register(context)
    }
  }

  private suspend fun registerToken(service: PushService, token: String) {
    runCatching { core.core().registerPush(service, token) }
      .onSuccess {
        maybePendingToken = null
        PushRegistrationRetry.onRegistered(context)
        Log.i(TAG, "push token registered: $service")
      }
      .onFailure {
        maybePendingToken = service to token
        Log.w(TAG, "push registration failed: $service ${it.javaClass.simpleName}")
        // Only a network failure passes by itself; anything else waits for the next foreground.
        if (it is TeamCraftException.Network && !isSigningOut) PushRegistrationRetry.schedule(context)
      }
  }

  /**
   * Starts opening a wake-up in the app's scope (or returns the run already going for the same
   * payload), so it isn't cancelled when a receiver's time is up.
   */
  fun startWakeUp(payload: ByteArray, isVisible: Boolean): Deferred<Unit> {
    val key = payload.contentHashCode().toString() + ":" + payload.size
    val run = running.computeIfAbsent(key) {
      scope.async(start = CoroutineStart.LAZY) {
        try {
          onWakeUp(payload, isVisible)
        } finally {
          running.remove(key)
        }
      }
    }
    run.start()
    return run
  }

  /**
   * Opens a wake-up and shows its notification. Never logs the payload or the text. `isVisible`:
   * the push service delivered it as one that must show something (FCM high priority: never a
   * withdrawal); if it can't be opened at all, a plain "New message" is shown instead of nothing.
   */
  suspend fun onWakeUp(payload: ByteArray, isVisible: Boolean) {
    if (isSigningOut) return
    val core = runCatching { core.core() }.getOrElse {
      Log.w(TAG, "wake-up while no account is open: ${it.javaClass.simpleName}")
      if (isVisible && !isSigningOut) handler.handleUnreadable() else handler.recordUnreadable()
      return
    }
    runCatching {
      // Keywords and the first name are matched on the device (the server can't read the text).
      core.setMentionSettings(mentionPrefs.settings.value)
      core.setNotificationContent(contentPrefs.content.value)
      core.notificationForPush(payload, utcOffsetMinutes())
    }
      // Checked again: sign-out may have started while the wake-up was being opened.
      .onSuccess {
        if (isSigningOut) return@onSuccess
        handler.handle(it)
        scheduleRetry(payload, it, attempt = 1)
      }
      .onFailure {
        Log.w(TAG, "wake-up could not be opened: ${it.javaClass.simpleName}")
        if (isVisible && !isSigningOut) handler.handleUnreadable() else handler.recordUnreadable()
      }
  }

  /**
   * Opens a wake-up again whose message was shown as a placeholder ([PushRetry]) and fills in the
   * notification, silently and under the same ID; schedules the next attempt while it still can't
   * be read. Shows nothing new: a notification read or swiped away meanwhile stays gone.
   */
  suspend fun retryWakeUp(payload: ByteArray, attempt: Int) {
    if (isSigningOut) return
    val core = runCatching { core.core() }.getOrNull() ?: return
    runCatching {
      core.setMentionSettings(mentionPrefs.settings.value)
      core.setNotificationContent(contentPrefs.content.value)
      core.notificationForPushRetry(payload, utcOffsetMinutes())
    }
      .onSuccess {
        if (isSigningOut) return@onSuccess
        handler.handleRetry(it)
        scheduleRetry(payload, it, attempt = attempt + 1)
      }
      .onFailure { Log.w(TAG, "wake-up retry failed: ${it.javaClass.simpleName}") }
  }

  private fun scheduleRetry(payload: ByteArray, shown: PushNotification, attempt: Int) {
    if (!PushRetry.isWanted(shown, attempt) || !handler.isShowing(shown.conversationId)) return
    // The gap is logged, never the content (notifications.md, "Mobile push delivery").
    Log.i(TAG, "message not read in time (${shown.maybeGap}); retry $attempt scheduled")
    WakeUpWork.enqueueRetry(context, payload, attempt)
  }

  private companion object {
    const val TAG = "TeamCraftPush"
    const val MS_PER_MINUTE = 60_000

    /** The device's offset from UTC now, for the core's DND check. */
    fun utcOffsetMinutes(): Short =
      (TimeZone.getDefault().getOffset(System.currentTimeMillis()) / MS_PER_MINUTE).toShort()
  }
}
