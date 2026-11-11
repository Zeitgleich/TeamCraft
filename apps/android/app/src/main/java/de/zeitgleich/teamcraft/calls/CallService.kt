package de.zeitgleich.teamcraft.calls

import android.app.Notification
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.media.projection.MediaProjectionManager
import android.os.Build
import android.os.IBinder
import android.util.Log
import androidx.core.content.IntentCompat
import androidx.core.app.NotificationCompat
import androidx.core.app.Person
import androidx.core.app.ServiceCompat
import androidx.core.content.ContextCompat
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.TeamCraftApplication
import de.zeitgleich.teamcraft.notifications.NotificationChannels

/**
 * Keeps the process, the microphone, the camera and a screen share alive while a call runs, with
 * the ongoing-call notification (CallStyle) that lets the user return to it, hang up, or stop
 * sharing the screen (the share goes on behind other apps, so it has to end from there). A screen
 * share's projection is taken here: Android 14+ hands it out only to a service that already runs
 * in the foreground as `mediaProjection`.
 */
class CallService : Service() {
  override fun onBind(intent: Intent?): IBinder? = null

  override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
    if (intent?.action == ACTION_HANG_UP) {
      TeamCraftApplication.graph(this).calls.hangUp()
      return START_NOT_STICKY
    }
    if (intent?.action == ACTION_STOP_SHARING) {
      TeamCraftApplication.graph(this).calls.stopScreenShare()
      return START_NOT_STICKY
    }
    val hasCamera = intent?.getBooleanExtra(EXTRA_CAMERA, false) == true
    val maybeConsent = intent?.let { IntentCompat.getParcelableExtra(it, EXTRA_PROJECTION_CONSENT, Intent::class.java) }
    val hasScreen = intent?.getBooleanExtra(EXTRA_SCREEN, false) == true || maybeConsent != null
    ServiceCompat.startForeground(this, NOTIFICATION_ID, notification(hasScreen), types(hasCamera, hasScreen))
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

  private fun notification(isSharingScreen: Boolean): Notification {
    val open = PendingIntent.getActivity(
      this,
      0,
      Intent(this, CallActivity::class.java),
      PendingIntent.FLAG_IMMUTABLE,
    )
    val hangUp = PendingIntent.getService(
      this,
      1,
      Intent(this, CallService::class.java).setAction(ACTION_HANG_UP),
      PendingIntent.FLAG_IMMUTABLE,
    )
    val caller = Person.Builder().setName(getString(R.string.call_ongoing)).build()
    val builder = NotificationCompat.Builder(this, NotificationChannels.CALLS)
      .setSmallIcon(R.drawable.ic_notification)
      .setContentIntent(open)
      .setOngoing(true)
      .setCategory(NotificationCompat.CATEGORY_CALL)
      .setStyle(NotificationCompat.CallStyle.forOngoingCall(caller, hangUp))
    if (isSharingScreen) builder.addAction(stopSharingAction())
    return builder.build()
  }

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

  companion object {
    private const val NOTIFICATION_ID = 2001
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

    fun stop(context: Context) {
      context.stopService(Intent(context, CallService::class.java))
    }
  }
}
