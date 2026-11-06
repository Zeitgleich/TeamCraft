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
 * A `sync` wake-up said messages came that this phone got no visible push for (the user was at the
 * desktop, a channel notifies on mentions only): fetches them in the background once a network is
 * there, so they are on screen at once when the app opens (notifications.md, "Sync wake-ups").
 * Shows nothing. Expedited where the quota allows (a normal-priority wake-up usually has none, so it
 * runs as regular work); one run catches up everything, so wake-ups in between are one run.
 */
object CatchUpWork {
  private const val WORK_NAME = "catch-up"
  /** How long one run syncs before it gives up and tries again later; below WorkManager's limit. */
  private const val WORK_BUDGET_MS = 2 * 60_000L
  private const val BACKOFF_SECONDS = 30L
  private const val FOREGROUND_ID = 2006

  /** After this many failed runs the next wake-up or foreground takes over. */
  const val MAX_ATTEMPTS = 5

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
      // The core stops the sync loop at the end unless the app came to the foreground meanwhile.
      val isDone = runCatching {
        val core = graph.core.core()
        if (core.accountState() is AccountState.Ready) core.catchUpInBackground(WORK_BUDGET_MS.toULong())
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
        .setContentTitle(applicationContext.getString(R.string.catching_up_in_background))
        .setSilent(true)
        .build()
      // Only used before Android 12, where a foreground service needs no declared type.
      return ForegroundInfo(FOREGROUND_ID, notification)
    }
  }
}
