package de.zeitgleich.teamcraft.core

import android.content.Context
import androidx.work.Constraints
import androidx.work.CoroutineWorker
import androidx.work.ExistingWorkPolicy
import androidx.work.NetworkType
import androidx.work.OneTimeWorkRequestBuilder
import androidx.work.WorkManager
import androidx.work.WorkerParameters
import de.zeitgleich.teamcraft.TeamCraftApplication
import java.util.concurrent.TimeUnit
import uniffi.teamcraft_ffi.AccountState

/**
 * Wakes the app for the next scheduled message (docs/scheduled-messages.md), so a phone in a pocket
 * sends it without being opened. Best effort: WorkManager runs it at or after the time, once a
 * network is there, and Doze may defer it. The lists say that a message goes out when a device of
 * the user is online at or after its time; this makes the phone one of them more often. Nothing
 * here decides anything: the run only keeps the core's sync loop going, whose upkeep sends what is
 * due.
 */
object ScheduledSendWork {
  private const val WORK_NAME = "scheduled-send"
  /** How long one run syncs before it gives up; below WorkManager's limit. */
  private const val WORK_BUDGET_MS = 3 * 60_000L

  /** The wait until [dueAtMs] (both on this device's clock); at once when it has passed. */
  fun delayMs(dueAtMs: Long, nowMs: Long): Long = (dueAtMs - nowMs).coerceAtLeast(0)

  /** Plans the run for the next message, or takes the plan back when nothing waits. */
  suspend fun plan(context: Context, repository: CoreRepository) {
    val maybeNext = runCatching {
      val answer = decodeAnswer(repository.core().query(ChatQueries.scheduledMessages()))
      answer.overview?.maybeNextDueAt
    }.getOrNull()
    val work = WorkManager.getInstance(context)
    if (maybeNext == null) {
      work.cancelUniqueWork(WORK_NAME)
      return
    }
    val request = OneTimeWorkRequestBuilder<Worker>()
      .setInitialDelay(delayMs(maybeNext, System.currentTimeMillis()), TimeUnit.MILLISECONDS)
      .setConstraints(Constraints.Builder().setRequiredNetworkType(NetworkType.CONNECTED).build())
      .build()
    work.enqueueUniqueWork(WORK_NAME, ExistingWorkPolicy.REPLACE, request)
  }

  class Worker(context: Context, parameters: WorkerParameters) : CoroutineWorker(context, parameters) {
    override suspend fun doWork(): Result {
      val graph = TeamCraftApplication.graph(applicationContext)
      if (!graph.auth.isSignedIn.value) return Result.success()
      runCatching {
        val core = graph.core.core()
        if (core.accountState() is AccountState.Ready) core.flushPendingWork(WORK_BUDGET_MS.toULong())
      }
      // The next message, or this one again if it could not go out.
      plan(applicationContext, graph.core)
      return Result.success()
    }
  }
}
