package de.zeitgleich.teamcraft.push

import android.content.Context
import java.util.concurrent.TimeUnit
import androidx.work.Constraints
import androidx.work.CoroutineWorker
import androidx.work.ExistingWorkPolicy
import androidx.work.NetworkType
import androidx.work.OneTimeWorkRequestBuilder
import androidx.work.WorkManager
import androidx.work.WorkerParameters
import de.zeitgleich.teamcraft.TeamCraftApplication

/**
 * Sends a push token the server didn't get (no network in Doze, server briefly down) from the
 * background, once a network is there, with backoff. Before, it waited for the app to come to the
 * foreground: a UnifiedPush endpoint the distributor handed out while the phone dozed never reached
 * the server, which kept pushing to the old one, and notifications stopped until the app was opened
 * (issue #826). Nothing is stored here: the attempt asks the push service for its token again, which
 * arrives through [PushManager.onToken] like any other.
 */
object PushRegistrationRetry {
  private const val WORK_NAME = "push-token-register"
  private const val FILE = "push_registration_retry"
  private const val KEY_ATTEMPTS = "attempts"

  /** First wait; it doubles per attempt up to [MAX_DELAY_MS] and keeps trying there. */
  private const val INITIAL_DELAY_MS = 30_000L
  private const val MAX_DELAY_MS = 60 * 60 * 1000L

  /** The registration reached the server. */
  fun onRegistered(context: Context) {
    prefs(context).edit().putInt(KEY_ATTEMPTS, 0).apply()
    WorkManager.getInstance(context).cancelUniqueWork(WORK_NAME)
  }

  /** The registration failed for a reason that passes (network); tries again in the background. */
  fun schedule(context: Context) {
    val prefs = prefs(context)
    val attempt = prefs.getInt(KEY_ATTEMPTS, 0) + 1
    prefs.edit().putInt(KEY_ATTEMPTS, attempt).apply()
    val request = OneTimeWorkRequestBuilder<Worker>()
      .setInitialDelay(delayMs(attempt), TimeUnit.MILLISECONDS)
      .setConstraints(Constraints.Builder().setRequiredNetworkType(NetworkType.CONNECTED).build())
      .build()
    // A newer failure decides the wait; a pending attempt is replaced, never doubled.
    WorkManager.getInstance(context).enqueueUniqueWork(WORK_NAME, ExistingWorkPolicy.REPLACE, request)
  }

  /** The wait before attempt [attempt] (1-based). */
  fun delayMs(attempt: Int): Long {
    val exponent = (attempt - 1).coerceIn(0, 20)
    return (INITIAL_DELAY_MS shl exponent).coerceAtMost(MAX_DELAY_MS)
  }

  private fun prefs(context: Context) = context.getSharedPreferences(FILE, Context.MODE_PRIVATE)

  /** One attempt; a failure schedules the next one through [PushManager]. */
  class Worker(context: Context, parameters: WorkerParameters) : CoroutineWorker(context, parameters) {
    override suspend fun doWork(): Result {
      val graph = TeamCraftApplication.graph(applicationContext)
      if (graph.auth.isSignedIn.value) graph.push.retryRegistration()
      return Result.success()
    }
  }
}
