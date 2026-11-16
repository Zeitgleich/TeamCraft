package de.zeitgleich.teamcraft.calls

import android.Manifest
import android.app.Notification
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.content.pm.ServiceInfo
import android.media.projection.MediaProjectionManager
import android.os.Build
import android.os.IBinder
import android.os.SystemClock
import android.util.Log
import androidx.core.content.IntentCompat
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.app.Person
import androidx.core.app.ServiceCompat
import androidx.core.content.ContextCompat
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.mapLatest
import kotlinx.coroutines.launch
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.TeamCraftApplication
import de.zeitgleich.teamcraft.notifications.NotificationChannels
import uniffi.teamcraft_ffi.CallPhase

/**
 * Keeps the process, the microphone, the camera and a screen share alive while a call runs, with
 * the ongoing-call notification (CallStyle) that lets the user return to it, mute, hang up, or stop
 * sharing the screen (the share goes on behind other apps, so it has to end from there). The
 * notification names the call and counts its time; on Android 16 it asks to be a Live Update, so
 * the status bar's chip shows the time too (#2067). A screen share's projection is taken here:
 * Android 14+ hands it out only to a service that already runs in the foreground as
 * `mediaProjection`.
 */
class CallService : Service() {
  /** What the notification shows, kept in step with the call while the service runs. */
  private var shown = Shown()

  /**
   * The service's own work, on the main thread like its lifecycle: an update of the notification
   * can't run between `stopService` and [onDestroy], or before [onStartCommand] made it a
   * foreground service.
   */
  private val serviceScope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)

  /** In the foreground with its notification: only then may it post that notification again. */
  private var isForeground = false

  override fun onBind(intent: Intent?): IBinder? = null

  override fun onCreate() {
    super.onCreate()
    watchCall()
  }

  override fun onDestroy() {
    isForeground = false
    serviceScope.cancel()
    super.onDestroy()
  }

  override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
    if (intent?.action == ACTION_HANG_UP) {
      TeamCraftApplication.graph(this).calls.hangUpFromSystem()
      return START_NOT_STICKY
    }
    if (intent?.action == ACTION_TOGGLE_MUTE) {
      val calls = TeamCraftApplication.graph(this).calls
      calls.callState.value?.let { calls.setMuted(!it.isMuted) }
      return START_NOT_STICKY
    }
    if (intent?.action == ACTION_STOP_SHARING) {
      TeamCraftApplication.graph(this).calls.stopScreenShare()
      return START_NOT_STICKY
    }
    val hasCamera = intent?.getBooleanExtra(EXTRA_CAMERA, false) == true
    val maybeConsent = intent?.let { IntentCompat.getParcelableExtra(it, EXTRA_PROJECTION_CONSENT, Intent::class.java) }
    val hasScreen = intent?.getBooleanExtra(EXTRA_SCREEN, false) == true || maybeConsent != null
    shown = shown.copy(isSharingScreen = hasScreen)
    ServiceCompat.startForeground(this, NOTIFICATION_ID, notification(shown), types(hasCamera, hasScreen))
    isForeground = true
    if (intent != null && maybeConsent != null) {
      takeProjection(intent.getIntExtra(EXTRA_PROJECTION_RESULT, 0), maybeConsent)
    }
    return START_NOT_STICKY
  }

  private fun types(hasCamera: Boolean, hasScreen: Boolean): Int {
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.R) return 0
    var types = ServiceInfo.FOREGROUND_SERVICE_TYPE_PHONE_CALL or ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE
    if (hasCamera) types = types or ServiceInfo.FOREGROUND_SERVICE_TYPE_CAMERA
    if (hasScreen) types = types or ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PROJECTION
    return types
  }

  /** The user's consent becomes the projection, now that the service runs as `mediaProjection`. */
  private fun takeProjection(resultCode: Int, consent: Intent) {
    val calls = TeamCraftApplication.graph(this).calls
    val maybeProjection = try {
      getSystemService(MediaProjectionManager::class.java).getMediaProjection(resultCode, consent)
    } catch (error: Exception) {
      // A consent used twice (Android 14+ allows one projection per consent) or revoked meanwhile.
      Log.w(TAG, "no projection for the consent", error)
      null
    }
    calls.onScreenProjection(maybeProjection)
  }

  /**
   * Follows the call's name, mute and clock into the notification. Posted again only when one of
   * them changed (the clock itself is the system's chronometer, no ticking here).
   */
  @OptIn(ExperimentalCoroutinesApi::class)
  private fun watchCall(): Job {
    val calls = TeamCraftApplication.graph(this).calls
    return serviceScope.launch {
      combine(calls.callState, calls.connectedAtMs) { maybeState, maybeConnectedAt -> maybeState to maybeConnectedAt }
        .mapLatest { (maybeState, maybeConnectedAt) ->
          val isRunning = maybeState != null && maybeState.phase != CallPhase.ENDED
          shown.copy(
            maybeName = maybeState?.conversationId?.let { calls.displayName(it) },
            isMuted = maybeState?.isMuted == true,
            // The chronometer counts from a wall-clock time; the controller keeps the uptime.
            maybeStartedAtMs = maybeConnectedAt?.takeIf { isRunning }?.let {
              System.currentTimeMillis() - (SystemClock.elapsedRealtime() - it)
            },
          )
        }
        .distinctUntilChanged { old, new ->
          old.maybeName == new.maybeName && old.isMuted == new.isMuted &&
            (old.maybeStartedAtMs == null) == (new.maybeStartedAtMs == null)
        }
        .collect { next ->
          shown = next.copy(isSharingScreen = shown.isSharingScreen)
          // An ended call stops the service right away: its notification goes with it.
          if (calls.callState.value.let { it != null && it.phase != CallPhase.ENDED }) repost()
        }
    }
  }

  /**
   * The notification again, as it is now (without notification permission it can't change). Only
   * while the service is in the foreground: Android refuses a CallStyle notification of anything
   * else with an exception, which ended the app (and the call with it) when a call ended or
   * reconnected while the name or the mute changed.
   */
  private fun repost() {
    if (!isForeground) return
    val isAllowed = Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU ||
      ContextCompat.checkSelfPermission(this, Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED
    if (!isAllowed) return
    // The system may still refuse it in the moment the service leaves the foreground: the
    // notification is only a nicer copy of the one shown, so it stays as it was.
    runCatching { NotificationManagerCompat.from(this).notify(NOTIFICATION_ID, notification(shown)) }
      .onFailure { Log.w(TAG, "call notification not updated", it) }
  }

  private fun notification(shown: Shown): Notification {
    val open = PendingIntent.getActivity(
      this,
      0,
      Intent(this, CallActivity::class.java),
      PendingIntent.FLAG_IMMUTABLE,
    )
    val name = shown.maybeName ?: getString(R.string.call_ongoing)
    val caller = Person.Builder().setName(name).build()
    val builder = NotificationCompat.Builder(this, NotificationChannels.CALLS)
      .setSmallIcon(R.drawable.ic_notification)
      .setContentTitle(name)
      .setContentIntent(open)
      .setOngoing(true)
      .setCategory(NotificationCompat.CATEGORY_CALL)
      .setStyle(NotificationCompat.CallStyle.forOngoingCall(caller, hangUpIntent(this)))
      // Android 16: a Live Update, its status bar chip with the call's time.
      .setRequestPromotedOngoing(true)
      .addAction(muteAction(shown.isMuted))
    shown.maybeStartedAtMs?.let { startedAt ->
      builder.setWhen(startedAt).setShowWhen(true).setUsesChronometer(true)
    }
    if (shown.isSharingScreen) builder.addAction(stopSharingAction())
    return builder.build()
  }

  /** "Mute" or "Unmute", as the call screen's button. */
  private fun muteAction(isMuted: Boolean): NotificationCompat.Action =
    NotificationCompat.Action.Builder(
      if (isMuted) R.drawable.ic_mic_off else R.drawable.ic_mic,
      getString(if (isMuted) R.string.call_unmute else R.string.call_mute),
      toggleMuteIntent(this),
    ).build()

  /** "Stop sharing" next to "Hang up", while the screen is shared or the share is starting. */
  private fun stopSharingAction(): NotificationCompat.Action {
    val stop = PendingIntent.getService(
      this,
      2,
      Intent(this, CallService::class.java).setAction(ACTION_STOP_SHARING),
      PendingIntent.FLAG_IMMUTABLE,
    )
    return NotificationCompat.Action.Builder(R.drawable.ic_stop, getString(R.string.call_stop_sharing), stop).build()
  }

  /** What the notification shows. */
  private data class Shown(
    val maybeName: String? = null,
    val isMuted: Boolean = false,
    /** Wall-clock start of the call's time; `null` until it connected. */
    val maybeStartedAtMs: Long? = null,
    val isSharingScreen: Boolean = false,
  )

  companion object {
    private const val NOTIFICATION_ID = 2001
    private const val ACTION_TOGGLE_MUTE = "de.zeitgleich.teamcraft.calls.TOGGLE_MUTE"
    private const val ACTION_HANG_UP = "de.zeitgleich.teamcraft.calls.HANG_UP"
    private const val ACTION_STOP_SHARING = "de.zeitgleich.teamcraft.calls.STOP_SHARING"
    private const val EXTRA_CAMERA = "camera"
    private const val EXTRA_SCREEN = "screen"
    private const val EXTRA_PROJECTION_RESULT = "projection_result"
    private const val EXTRA_PROJECTION_CONSENT = "projection_consent"
    private const val TAG = "TeamCraftCalls"

    /** Starts the service, or updates what it keeps alive (the camera and the screen while they send). */
    fun start(context: Context, hasCamera: Boolean, hasScreen: Boolean) {
      val intent = Intent(context, CallService::class.java)
        .putExtra(EXTRA_CAMERA, hasCamera)
        .putExtra(EXTRA_SCREEN, hasScreen)
      ContextCompat.startForegroundService(context, intent)
    }

    /**
     * Keeps the screen alive too and turns the user's consent (the system dialog's result) into
     * the projection, which goes to [CallController.onScreenProjection].
     */
    fun shareScreen(context: Context, hasCamera: Boolean, resultCode: Int, consent: Intent) {
      val intent = Intent(context, CallService::class.java)
        .putExtra(EXTRA_CAMERA, hasCamera)
        .putExtra(EXTRA_PROJECTION_RESULT, resultCode)
        .putExtra(EXTRA_PROJECTION_CONSENT, consent)
      ContextCompat.startForegroundService(context, intent)
    }

    /** Hangs up from outside the call screen: the notification and picture-in-picture. */
    fun hangUpIntent(context: Context): PendingIntent = PendingIntent.getService(
      context,
      1,
      Intent(context, CallService::class.java).setAction(ACTION_HANG_UP),
      PendingIntent.FLAG_IMMUTABLE,
    )

    /** Mutes or unmutes from outside the call screen: the notification and picture-in-picture. */
    fun toggleMuteIntent(context: Context): PendingIntent = PendingIntent.getService(
      context,
      3,
      Intent(context, CallService::class.java).setAction(ACTION_TOGGLE_MUTE),
      PendingIntent.FLAG_IMMUTABLE,
    )

    fun stop(context: Context) {
      context.stopService(Intent(context, CallService::class.java))
    }
  }
}
