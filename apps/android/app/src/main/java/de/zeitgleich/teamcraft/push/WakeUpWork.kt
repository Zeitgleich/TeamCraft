package de.zeitgleich.teamcraft.push

import android.content.Context
import java.util.Base64
import java.util.concurrent.TimeUnit
import androidx.core.app.NotificationCompat
import androidx.work.Constraints
import androidx.work.CoroutineWorker
import androidx.work.ExistingWorkPolicy
import androidx.work.ForegroundInfo
import androidx.work.NetworkType
import androidx.work.OneTimeWorkRequestBuilder
import androidx.work.OutOfQuotaPolicy
import androidx.work.WorkManager
import androidx.work.WorkerParameters
import androidx.work.workDataOf
import kotlinx.coroutines.withTimeoutOrNull
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.TeamCraftApplication
import de.zeitgleich.teamcraft.notifications.NotificationChannels
import de.zeitgleich.teamcraft.notifications.PushRetry

/**
 * Wake-ups that don't finish within the push service's time budget (FCM gives a high-priority
 * message about 10 s, a broadcast receiver as much) continue as expedited work, so a slow fetch
 * neither gets the process killed nor loses the notification. A wake-up whose message could not be
 * read in time is opened again later ([PushRetry]) to fill in the notification it showed.
 */
object WakeUpWork {
  /** What a receiver waits for itself before it hands the wake-up over. */
  const val RECEIVER_BUDGET_MS = 8_000L

  private const val KEY_PAYLOAD = "payload"
  private const val KEY_IS_VISIBLE = "is_visible"
  private const val KEY_ATTEMPT = "attempt"
  private const val KEY_RETRY_OF = "retry_of"
  private const val KEY_ARRIVED_AT = "arrived_at"
  private const val RETRY_TAG = "wake-up-retry"
  private const val HOLD_END_WORK = "wake-up-hold-end"
  private const val HOLD_END_AGAIN_MS = 30_000L
  private const val WORK_PREFIX = "wake-up-"
  private const val RETRY_PREFIX = "wake-up-retry-"
  /** Past [PushRetry.SETTLE_BOUND_MS], so the wait for a message's text ends inside the job. */
  private const val WORK_BUDGET_MS = 150_000L
  private const val FOREGROUND_ID = 2003

  /**
   * Handles a wake-up: waits up to [RECEIVER_BUDGET_MS] for it, and if it is still running then,
   * lets expedited work wait for the rest (the same run, not a second one, while the process lives).
   */
  suspend fun handle(context: Context, payload: ByteArray, isVisible: Boolean) {
    val running = TeamCraftApplication.graph(context).push.startWakeUp(payload, isVisible)
    if (withTimeoutOrNull(RECEIVER_BUDGET_MS) { running.await() } == null) enqueue(context, payload, isVisible)
  }

  /**
   * Opens the wake-up again after [PushRetry.delayMs] once a network is there; `attempt` 1 is the
   * first retry. One per payload and attempt.
   */
  fun enqueueRetry(
    context: Context,
    payload: ByteArray,
    attempt: Int,
    retryOf: RetryOf,
    arrivedAtMs: Long,
  ) {
    val encoded = Base64.getEncoder().encodeToString(payload)
    val request = OneTimeWorkRequestBuilder<RetryWorker>()
      .setInputData(workDataOf(KEY_PAYLOAD to encoded, KEY_ATTEMPT to attempt, KEY_RETRY_OF to retryOf.name, KEY_ARRIVED_AT to arrivedAtMs))
      .setInitialDelay(PushRetry.delayMs(attempt), TimeUnit.MILLISECONDS)
      .setConstraints(Constraints.Builder().setRequiredNetworkType(NetworkType.CONNECTED).build())
      .addTag(RETRY_TAG)
      .build()
    WorkManager.getInstance(context).enqueueUniqueWork(retryName(encoded, attempt), ExistingWorkPolicy.KEEP, request)
  }

  private fun retryName(encoded: String, attempt: Int): String = RETRY_PREFIX + Integer.toHexString(encoded.hashCode()) + "-" + attempt

  /**
   * The retries waiting now; one per chain, since a chain's next attempt is enqueued by the last.
   * A query of WorkManager's own database, only when a chain starts, off the main thread.
   */
  fun pendingRetries(context: Context): Int =
    runCatching { WorkManager.getInstance(context).getWorkInfosByTag(RETRY_TAG).get().count { !it.state.isFinished } }
      .getOrDefault(0)

  /** Takes back `attempt` of this wake-up's retries if it hasn't run yet. */
  fun cancelRetry(context: Context, payload: ByteArray, attempt: Int) {
    WorkManager.getInstance(context).cancelUniqueWork(retryName(Base64.getEncoder().encodeToString(payload), attempt))
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
      val running = TeamCraftApplication.graph(applicationContext).push.resumeWakeUp(payload, isVisible)
      withTimeoutOrNull(WORK_BUDGET_MS) { running.await() }
      // One that did not open schedules its own retries (PushManager.onNotOpened).
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

  /** A later try to read a message shown as a placeholder (see [enqueueRetry]). */
  class RetryWorker(context: Context, parameters: WorkerParameters) : CoroutineWorker(context, parameters) {
    override suspend fun doWork(): Result {
      val encoded = inputData.getString(KEY_PAYLOAD) ?: return Result.failure()
      val payload = runCatching { Base64.getDecoder().decode(encoded) }.getOrNull() ?: return Result.failure()
      val attempt = inputData.getInt(KEY_ATTEMPT, PushRetry.MAX_ATTEMPTS)
      val retryOf = RetryOf.entries.firstOrNull { it.name == inputData.getString(KEY_RETRY_OF) } ?: RetryOf.PLACEHOLDER
      // Missing only for a job from before this was kept: too old to alert.
      val arrivedAtMs = inputData.getLong(KEY_ARRIVED_AT, -1)
      val push = TeamCraftApplication.graph(applicationContext).push
      withTimeoutOrNull(WORK_BUDGET_MS) { push.retryWakeUp(payload, attempt, retryOf, arrivedAtMs) }
        ?: push.onRetryTimedOut(payload, attempt, retryOf, arrivedAtMs)
      // The next attempt, if any, is its own work: WorkManager's retry would not wait for a network.
      return Result.success()
    }
  }

  /** Ends the holds past their bound; held alerts are from earlier versions, this one posts none. */
  class HoldEndWorker(context: Context, parameters: WorkerParameters) : CoroutineWorker(context, parameters) {
    override suspend fun doWork(): Result {
      val isHoldLeft = TeamCraftApplication.graph(applicationContext).push.endHolds()
      if (isHoldLeft) {
        val request = OneTimeWorkRequestBuilder<HoldEndWorker>()
          .setInitialDelay(HOLD_END_AGAIN_MS, TimeUnit.MILLISECONDS)
          .build()
        // This job is still running under the name: the next one takes over when it ended.
        WorkManager.getInstance(applicationContext).enqueueUniqueWork(HOLD_END_WORK, ExistingWorkPolicy.APPEND_OR_REPLACE, request)
      }
      return Result.success()
    }
  }
}
