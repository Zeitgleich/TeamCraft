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
import kotlinx.coroutines.withTimeoutOrNull
import de.zeitgleich.teamcraft.core.CoreRepository
import de.zeitgleich.teamcraft.notifications.CarProjection
import de.zeitgleich.teamcraft.notifications.MentionPrefs
import de.zeitgleich.teamcraft.notifications.NotificationContentPrefs
import de.zeitgleich.teamcraft.notifications.PushHandler
import de.zeitgleich.teamcraft.notifications.PushPath
import de.zeitgleich.teamcraft.notifications.PushRetry
import uniffi.teamcraft_ffi.PushNotification
import uniffi.teamcraft_ffi.PushService
import uniffi.teamcraft_ffi.TeamCraft
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
  private val car: CarProjection,
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
      onNotOpened(payload, isVisible, isSignedOut = it is IllegalStateException)
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
        handler.handle(it, payload, isVisible)
        if (handler.isPlaceholderShowing(it) && PushRetry.isWanted(it, attempt = 1)) readSoon(core, payload, it)
      }
      .onFailure {
        Log.w(TAG, "wake-up could not be opened: ${it.javaClass.simpleName}")
        onNotOpened(payload, isVisible, isSignedOut = false)
      }
  }

  /**
   * Not opened: says "New message" if it must show something, and tries again later, since what
   * kept it from opening (a busy or locked store, a process just starting) may pass. Without this
   * the message would stay unknown until the app is opened.
   */
  private fun onNotOpened(payload: ByteArray, isVisible: Boolean, isSignedOut: Boolean) {
    val wasAnnounced = isVisible && !isSigningOut
    if (wasAnnounced) handler.handleUnreadable() else handler.recordUnreadable()
    if (!isSigningOut && !isSignedOut) {
      WakeUpWork.enqueueRetry(context, payload, attempt = 1, unopened = RetryOf.of(wasAnnounced))
    }
  }

  /**
   * The message of a wake-up was not read: keeps the later retries safe from a process death first,
   * then tries once more right away while its alert is held, so that it usually alerts once and
   * with the text. The hold is bounded, by [PushRetry.HOLD_TRY_MS] on top of the first try.
   */
  private suspend fun readSoon(core: TeamCraft, payload: ByteArray, shown: PushNotification) {
    Log.i(TAG, "message not read in time (${shown.maybeGap}); trying again")
    WakeUpWork.enqueueRetry(context, payload, attempt = 1)
    val again = withTimeoutOrNull(PushRetry.HOLD_TRY_MS) {
      runCatching { core.notificationForPushRetry(payload, utcOffsetMinutes()) }.getOrNull()
    }
    if (isSigningOut) return
    again?.let { handler.handleRetry(it, PushPath.FILLED_HOLD) }
    if (PushRetry.releasesHeldAlert(attempt = 0, car.isActive)) handler.releaseHeldAlert(shown)
  }

  /**
   * Opens a wake-up again ([PushRetry]): a message shown as a placeholder gets its text, or a
   * wake-up that didn't open at first ([RetryOf]) its notification. Schedules the next attempt
   * while it still can't be read, also after a failure: a store that was busy or a core that
   * didn't open is no reason to stop. A notification read or swiped away meanwhile stays gone.
   */
  suspend fun retryWakeUp(payload: ByteArray, attempt: Int, retryOf: RetryOf) {
    if (isSigningOut) return
    runCatching {
      val core = core.core()
      core.setMentionSettings(mentionPrefs.settings.value)
      core.setNotificationContent(contentPrefs.content.value)
      core.notificationForPushRetry(payload, utcOffsetMinutes())
    }
      .onSuccess {
        if (isSigningOut) return@onSuccess
        when (retryOf) {
          RetryOf.PLACEHOLDER -> handler.handleRetry(it, PushPath.FILLED_RETRY)
          RetryOf.UNOPENED_ANNOUNCED -> handler.handleOpenedLate(it, payload, wasAnnounced = true)
          RetryOf.UNOPENED_QUIET -> handler.handleOpenedLate(it, payload, wasAnnounced = false)
        }
        afterRetry(payload, it, attempt)
      }
      .onFailure {
        Log.w(TAG, "wake-up retry $attempt failed: ${it.javaClass.simpleName}")
        // Signed out: nothing to read any more, and nothing shows.
        if (it !is IllegalStateException) enqueueNext(payload, attempt, retryOf)
      }
  }

  /** An attempt didn't finish in its time: the next one takes over. */
  fun onRetryTimedOut(payload: ByteArray, attempt: Int, retryOf: RetryOf) {
    if (!isSigningOut) enqueueNext(payload, attempt, retryOf)
  }

  private fun afterRetry(payload: ByteArray, shown: PushNotification, attempt: Int) {
    if (!handler.isPlaceholderShowing(shown)) return
    if (PushRetry.releasesHeldAlert(attempt, car.isActive)) handler.releaseHeldAlert(shown)
    if (PushRetry.isWanted(shown, attempt + 1)) {
      // The gap is logged, never the content (notifications.md, "Mobile push delivery").
      Log.i(TAG, "message not read in time (${shown.maybeGap}); retry ${attempt + 1} scheduled")
      WakeUpWork.enqueueRetry(context, payload, attempt + 1)
    } else if (attempt >= PushRetry.MAX_ATTEMPTS) {
      handler.recordGaveUp()
    }
  }

  private fun enqueueNext(payload: ByteArray, attempt: Int, retryOf: RetryOf) {
    if (attempt < PushRetry.MAX_ATTEMPTS) {
      WakeUpWork.enqueueRetry(context, payload, attempt + 1, retryOf)
    } else {
      handler.recordGaveUp()
    }
  }

  private companion object {
    const val TAG = "TeamCraftPush"
    const val MS_PER_MINUTE = 60_000

    /** The device's offset from UTC now, for the core's DND check. */
    fun utcOffsetMinutes(): Short =
      (TimeZone.getDefault().getOffset(System.currentTimeMillis()) / MS_PER_MINUTE).toShort()
  }
}

/** What a retry of a wake-up is for, and whether something was said for it yet. */
enum class RetryOf {
  /** A message shown as a placeholder. */
  PLACEHOLDER,

  /** A wake-up that did not open; a generic "New message" went out for it. */
  UNOPENED_ANNOUNCED,

  /** A wake-up that did not open, and nothing showed (it may have been a withdrawal). */
  UNOPENED_QUIET,
  ;

  companion object {
    fun of(wasAnnounced: Boolean): RetryOf = if (wasAnnounced) UNOPENED_ANNOUNCED else UNOPENED_QUIET
  }
}
