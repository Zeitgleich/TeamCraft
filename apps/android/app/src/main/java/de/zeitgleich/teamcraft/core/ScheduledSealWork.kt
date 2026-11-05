package de.zeitgleich.teamcraft.core

import android.content.Context
import androidx.core.app.NotificationCompat
import androidx.work.BackoffPolicy
import androidx.work.Constraints
import androidx.work.CoroutineWorker
import androidx.work.ExistingWorkPolicy
import androidx.work.ForegroundInfo
import androidx.work.NetworkType
import androidx.work.OneTimeWorkRequestBuilder
import androidx.work.OutOfQuotaPolicy
import androidx.work.WorkManager
import androidx.work.WorkerParameters
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.TeamCraftApplication
import de.zeitgleich.teamcraft.notifications.NotificationChannels
import java.util.concurrent.TimeUnit
import uniffi.teamcraft_ffi.AccountState

/**
 * A push said the group of a scheduled message changed (docs/scheduled-messages.md): seals it again
 * under the current key, once a network is there, and tries again with backoff. Expedited where the
 * quota allows; Doze and App Standby may defer the rest. One run reads the whole list.
 */
object ScheduledSealWork {
  private const val WORK_NAME = "scheduled-seal"
  private const val BACKOFF_SECONDS = 30L
  private const val FOREGROUND_ID = 2005

  /** After this many runs the next wake-up or foreground takes over. */
  const val MAX_ATTEMPTS = 8

  fun enqueue(context: Context) {
    val request = OneTimeWorkRequestBuilder<Worker>()
      .setExpedited(OutOfQuotaPolicy.RUN_AS_NON_EXPEDITED_WORK_REQUEST)
      .setConstraints(Constraints.Builder().setRequiredNetworkType(NetworkType.CONNECTED).build())
      .setBackoffCriteria(BackoffPolicy.EXPONENTIAL, BACKOFF_SECONDS, TimeUnit.SECONDS)
      .build()
    WorkManager.getInstance(context).enqueueUniqueWork(WORK_NAME, ExistingWorkPolicy.KEEP, request)
  }

  /** Whether a failed run is tried again. */
  fun retries(attempt: Int): Boolean = attempt + 1 < MAX_ATTEMPTS

  class Worker(context: Context, parameters: WorkerParameters) : CoroutineWorker(context, parameters) {
    override suspend fun doWork(): Result {
      val graph = TeamCraftApplication.graph(applicationContext)
      if (!graph.auth.isSignedIn.value) return Result.success()
      val isDone = runCatching {
        val core = graph.core.core()
        if (core.accountState() is AccountState.Ready) core.sealScheduledMessages()
      }.isSuccess
      return when {
        isDone -> Result.success()
        retries(runAttemptCount) -> Result.retry()
        else -> Result.success()
      }
    }

    /** Before Android 12, expedited work runs as a foreground service with this notification. */
    override suspend fun getForegroundInfo(): ForegroundInfo {
      val notification = NotificationCompat.Builder(applicationContext, NotificationChannels.OTHER)
        .setSmallIcon(R.drawable.ic_notification)
        .setContentTitle(applicationContext.getString(R.string.outbox_sending))
        .setSilent(true)
        .build()
      return ForegroundInfo(FOREGROUND_ID, notification)
    }
  }
}
