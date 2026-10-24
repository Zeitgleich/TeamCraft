package de.zeitgleich.teamcraft.push

import android.content.Context
import java.util.Base64
import androidx.core.app.NotificationCompat
import androidx.work.CoroutineWorker
import androidx.work.ExistingWorkPolicy
import androidx.work.ForegroundInfo
import androidx.work.OneTimeWorkRequestBuilder
import androidx.work.OutOfQuotaPolicy
import androidx.work.WorkManager
import androidx.work.WorkerParameters
import androidx.work.workDataOf
import kotlinx.coroutines.withTimeoutOrNull
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.TeamCraftApplication
import de.zeitgleich.teamcraft.notifications.NotificationChannels

/**
 * Wake-ups that don't finish within the push service's time budget (FCM gives a high-priority
 * message about 10 s, a broadcast receiver as much) continue as expedited work, so a slow fetch
 * neither gets the process killed nor loses the notification.
 */
object WakeUpWork {
  /** What a receiver waits for itself before it hands the wake-up over. */
  const val RECEIVER_BUDGET_MS = 8_000L

  private const val KEY_PAYLOAD = "payload"
  private const val KEY_IS_VISIBLE = "is_visible"
  private const val WORK_PREFIX = "wake-up-"
  private const val WORK_BUDGET_MS = 60_000L
  private const val FOREGROUND_ID = 2003

  /**
   * Handles a wake-up: waits up to [RECEIVER_BUDGET_MS] for it, and if it is still running then,
   * lets expedited work wait for the rest (the same run, not a second one, while the process lives).
   */
  suspend fun handle(context: Context, payload: ByteArray, isVisible: Boolean) {
    val running = TeamCraftApplication.graph(context).push.startWakeUp(payload, isVisible)
    if (withTimeoutOrNull(RECEIVER_BUDGET_MS) { running.await() } == null) enqueue(context, payload, isVisible)
  }

  private fun enqueue(context: Context, payload: ByteArray, isVisible: Boolean) {
    val encoded = Base64.getEncoder().encodeToString(payload)
    val request = OneTimeWorkRequestBuilder<Worker>()
      .setInputData(workDataOf(KEY_PAYLOAD to encoded, KEY_IS_VISIBLE to isVisible))
      .setExpedited(OutOfQuotaPolicy.RUN_AS_NON_EXPEDITED_WORK_REQUEST)
      .build()
    // One unique name per payload: a wake-up handed over twice runs once.
    val name = WORK_PREFIX + Integer.toHexString(encoded.hashCode())
    WorkManager.getInstance(context).enqueueUniqueWork(name, ExistingWorkPolicy.KEEP, request)
  }

  class Worker(context: Context, parameters: WorkerParameters) : CoroutineWorker(context, parameters) {
    override suspend fun doWork(): Result {
      val encoded = inputData.getString(KEY_PAYLOAD) ?: return Result.failure()
      val payload = runCatching { Base64.getDecoder().decode(encoded) }.getOrNull() ?: return Result.failure()
      val isVisible = inputData.getBoolean(KEY_IS_VISIBLE, false)
      val running = TeamCraftApplication.graph(applicationContext).push.startWakeUp(payload, isVisible)
      withTimeoutOrNull(WORK_BUDGET_MS) { running.await() }
      // A wake-up is not retried: a later one, or the next sync, brings what it missed.
      return Result.success()
    }

    /** Before Android 12, expedited work runs as a foreground service with this notification. */
    override suspend fun getForegroundInfo(): ForegroundInfo {
      val notification = NotificationCompat.Builder(applicationContext, NotificationChannels.OTHER)
        .setSmallIcon(R.drawable.ic_notification)
        .setContentTitle(applicationContext.getString(R.string.push_fetching))
        .setSilent(true)
        .build()
      // Only used before Android 12, where a foreground service needs no declared type.
      return ForegroundInfo(FOREGROUND_ID, notification)
    }
  }
}
