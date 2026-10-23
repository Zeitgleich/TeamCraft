package de.zeitgleich.teamcraft.calls

import android.Manifest
import android.app.Notification
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.util.Log
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.app.Person
import androidx.core.content.ContextCompat
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.notifications.NotificationChannels
import de.zeitgleich.teamcraft.ui.MainActivity

/**
 * The incoming-call notification: CallStyle with answer and decline, a full-screen intent that
 * shows [CallActivity] over the lock screen, ringing on the "incoming calls" channel until the ring
 * ends (or the system drops it after [RING_TIMEOUT_MS]).
 */
class IncomingCallNotifier(private val context: Context) {
  private val manager = NotificationManagerCompat.from(context)

  fun show(ring: IncomingRing) {
    if (!canPost()) return
    val notification = if (ring.isVerified) callNotification(ring) else unverifiedNotification(ring)
    // Rings until answered, declined or cancelled, like a phone call.
    notification.flags = notification.flags or Notification.FLAG_INSISTENT
    try {
      manager.notify(NOTIFICATION_ID, notification)
    } catch (denied: SecurityException) {
      Log.w(TAG, "incoming call not shown: permission missing")
    }
  }

  fun cancel() {
    manager.cancel(NOTIFICATION_ID)
  }

  private fun callNotification(ring: IncomingRing): Notification {
    val caller = Person.Builder()
      .setName(ring.maybeCallerName ?: ring.displayName)
      .setImportant(true)
      .build()
    val fullScreen = activityIntent(CallActivity.ACTION_SHOW_INCOMING, ring.conversationId, REQUEST_SHOW)
    val answer = activityIntent(CallActivity.ACTION_ANSWER, ring.conversationId, REQUEST_ANSWER)
    val decline = PendingIntent.getBroadcast(
      context,
      REQUEST_DECLINE,
      Intent(context, CallActionReceiver::class.java).setAction(CallActionReceiver.ACTION_DECLINE),
      PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
    )
    return NotificationCompat.Builder(context, NotificationChannels.INCOMING_CALLS)
      .setSmallIcon(R.drawable.ic_notification)
      .setContentTitle(ring.displayName)
      .setContentText(context.getString(R.string.call_incoming))
      .setCategory(NotificationCompat.CATEGORY_CALL)
      .setPriority(NotificationCompat.PRIORITY_MAX)
      .setOngoing(true)
      .setVisibility(NotificationCompat.VISIBILITY_PUBLIC)
      .setContentIntent(fullScreen)
      // Android 14+ shows it as a heads-up instead if the user took the permission away.
      .setFullScreenIntent(fullScreen, true)
      .setStyle(NotificationCompat.CallStyle.forIncomingCall(caller, decline, answer))
      .setTimeoutAfter(RING_TIMEOUT_MS)
      .build()
  }

  /**
   * A ring for a conversation the device doesn't know: it rings, but answering needs the app
   * (tapping opens it), so the server alone can't make the phone join a call.
   */
  private fun unverifiedNotification(ring: IncomingRing): Notification {
    val open = PendingIntent.getActivity(
      context,
      REQUEST_OPEN,
      Intent(context, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP),
      PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
    )
    return NotificationCompat.Builder(context, NotificationChannels.INCOMING_CALLS)
      .setSmallIcon(R.drawable.ic_notification)
      .setContentTitle(ring.displayName)
      .setContentText(context.getString(R.string.call_incoming))
      .setCategory(NotificationCompat.CATEGORY_CALL)
      .setPriority(NotificationCompat.PRIORITY_MAX)
      .setVisibility(NotificationCompat.VISIBILITY_PRIVATE)
      .setContentIntent(open)
      .setAutoCancel(true)
      .setTimeoutAfter(RING_TIMEOUT_MS)
      .build()
  }

  private fun activityIntent(action: String, conversationId: String, requestCode: Int): PendingIntent =
    PendingIntent.getActivity(
      context,
      requestCode,
      Intent(context, CallActivity::class.java)
        .setAction(action)
        .putExtra(CallActivity.EXTRA_CONVERSATION_ID, conversationId)
        .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
      PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
    )

  private fun canPost(): Boolean =
    Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU ||
      ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED

  companion object {
    /** How long a ring lasts at most on this device if nothing ends it before. */
    const val RING_TIMEOUT_MS = 45_000L
    private const val NOTIFICATION_ID = 2002
    private const val REQUEST_SHOW = 10
    private const val REQUEST_ANSWER = 11
    private const val REQUEST_DECLINE = 12
    private const val REQUEST_OPEN = 13
    private const val TAG = "TeamCraftCalls"

    /**
     * Whether rings may take over the screen (Android 14+ lets users and stores revoke it; before
     * that every calling app may).
     */
    fun canUseFullScreenIntent(context: Context): Boolean {
      if (Build.VERSION.SDK_INT < Build.VERSION_CODES.UPSIDE_DOWN_CAKE) return true
      return context.getSystemService(NotificationManager::class.java)?.canUseFullScreenIntent() == true
    }
  }
}
