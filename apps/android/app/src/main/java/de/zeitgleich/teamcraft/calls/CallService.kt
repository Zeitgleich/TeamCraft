package de.zeitgleich.teamcraft.calls

import android.app.Notification
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import androidx.core.app.NotificationCompat
import androidx.core.app.Person
import androidx.core.app.ServiceCompat
import androidx.core.content.ContextCompat
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.TeamCraftApplication
import de.zeitgleich.teamcraft.notifications.NotificationChannels

/**
 * Keeps the process, the microphone and the camera alive while a call runs, with the ongoing-call
 * notification (CallStyle) that lets the user return to it or hang up.
 */
class CallService : Service() {
  override fun onBind(intent: Intent?): IBinder? = null

  override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
    if (intent?.action == ACTION_HANG_UP) {
      TeamCraftApplication.graph(this).calls.hangUp()
      return START_NOT_STICKY
    }
    val hasCamera = intent?.getBooleanExtra(EXTRA_CAMERA, false) == true
    val types = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
      val base = ServiceInfo.FOREGROUND_SERVICE_TYPE_PHONE_CALL or ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE
      if (hasCamera) base or ServiceInfo.FOREGROUND_SERVICE_TYPE_CAMERA else base
    } else {
      0
    }
    ServiceCompat.startForeground(this, NOTIFICATION_ID, notification(), types)
    return START_NOT_STICKY
  }

  private fun notification(): Notification {
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
    return NotificationCompat.Builder(this, NotificationChannels.CALLS)
      .setSmallIcon(R.drawable.ic_notification)
      .setContentIntent(open)
      .setOngoing(true)
      .setCategory(NotificationCompat.CATEGORY_CALL)
      .setStyle(NotificationCompat.CallStyle.forOngoingCall(caller, hangUp))
      .build()
  }

  companion object {
    private const val NOTIFICATION_ID = 2001
    private const val ACTION_HANG_UP = "de.zeitgleich.teamcraft.calls.HANG_UP"
    private const val EXTRA_CAMERA = "camera"

    /** Starts the service, or updates what it keeps alive (the camera while it sends). */
    fun start(context: Context, hasCamera: Boolean) {
      val intent = Intent(context, CallService::class.java).putExtra(EXTRA_CAMERA, hasCamera)
      ContextCompat.startForegroundService(context, intent)
    }

    fun stop(context: Context) {
      context.stopService(Intent(context, CallService::class.java))
    }
  }
}
