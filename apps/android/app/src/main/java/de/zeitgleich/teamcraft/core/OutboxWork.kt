package de.zeitgleich.teamcraft.core

import android.content.Context
import java.util.concurrent.TimeUnit
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
import uniffi.teamcraft_ffi.AccountState
import uniffi.teamcraft_ffi.PendingWorkItem

/**
 * Messages, files and queued actions that still have to reach the server after the app left the
 * foreground (issue #158). The core keeps them in its store, so they survive the process being
 * killed; this work sends them once a network is there, instead of waiting for the next foreground
 * or push wake-up. Expedited where the quota allows (Doze and App Standby defer the rest, no
 * wakelock), retried with backoff while something is still pending.
 */
object OutboxWork {
  private const val WORK_NAME = "outbox"
  /** How long one run syncs before it gives up and retries later; below WorkManager's limit. */
  private const val WORK_BUDGET_MS = 3 * 60_000L
  private const val BACKOFF_SECONDS = 30L
  /** After this many runs the next foreground sends what is left. */
  const val MAX_ATTEMPTS = 20
  private const val FOREGROUND_ID = 2004

  /** Schedules a run if none is waiting; one is enough, it sends everything. */
  fun enqueue(context: Context) {
    val request = OneTimeWorkRequestBuilder<Worker>()
      .setExpedited(OutOfQuotaPolicy.RUN_AS_NON_EXPEDITED_WORK_REQUEST)
      .setConstraints(Constraints.Builder().setRequiredNetworkType(NetworkType.CONNECTED).build())
      .setBackoffCriteria(BackoffPolicy.EXPONENTIAL, BACKOFF_SECONDS, TimeUnit.SECONDS)
      .build()
    WorkManager.getInstance(context).enqueueUniqueWork(WORK_NAME, ExistingWorkPolicy.KEEP, request)
  }

  /** What a run ends with, given what is still pending afterwards (`null`: nothing to send for). */
  fun outcome(maybeRemaining: PendingWorkItem?, attempt: Int): Outcome = when {
    maybeRemaining == null || maybeRemaining.isEmpty -> Outcome.Done
    attempt + 1 >= MAX_ATTEMPTS -> Outcome.Done
    else -> Outcome.Retry
  }

  enum class Outcome { Done, Retry }

  class Worker(context: Context, parameters: WorkerParameters) : CoroutineWorker(context, parameters) {
    override suspend fun doWork(): Result {
      val graph = TeamCraftApplication.graph(applicationContext)
      val maybeRemaining = if (graph.auth.isSignedIn.value) flush(graph.core) else null
      return when (outcome(maybeRemaining, runAttemptCount)) {
        Outcome.Done -> Result.success()
        Outcome.Retry -> Result.retry()
      }
    }

    /** Syncs until the queue is empty or the budget is used; the core stops the sync loop after it unless the app is visible. */
    private suspend fun flush(repository: CoreRepository): PendingWorkItem? = runCatching {
      val core = repository.core()
      if (core.accountState() !is AccountState.Ready) return null
      core.flushPendingWork(WORK_BUDGET_MS.toULong())
    }.getOrElse {
      // The core couldn't be opened or asked (signed out meanwhile, store busy): try again later.
      PendingWorkItem(sends = 1u, uploads = 0u, actions = 0u, isEmpty = false)
    }

    /** Before Android 12, expedited work runs as a foreground service with this notification. */
    override suspend fun getForegroundInfo(): ForegroundInfo {
      val notification = NotificationCompat.Builder(applicationContext, NotificationChannels.OTHER)
        .setSmallIcon(R.drawable.ic_notification)
        .setContentTitle(applicationContext.getString(R.string.outbox_sending))
        .setSilent(true)
        .build()
      // Only used before Android 12, where a foreground service needs no declared type.
      return ForegroundInfo(FOREGROUND_ID, notification)
    }
  }
}
