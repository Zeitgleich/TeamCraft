package de.zeitgleich.teamcraft.push

import android.content.Context
import android.util.Log
import java.util.concurrent.TimeUnit
import androidx.work.Constraints
import androidx.work.CoroutineWorker
import androidx.work.ExistingWorkPolicy
import androidx.work.NetworkType
import androidx.work.OneTimeWorkRequestBuilder
import androidx.work.WorkManager
import androidx.work.WorkerParameters
import org.unifiedpush.android.connector.UnifiedPush

/**
 * Registers with the UnifiedPush distributor again when the registration failed for a reason that
 * may pass (no network, the distributor busy or restarting) or the distributor dropped the app
 * (uninstalled, data cleared): with backoff, only while push is wanted (signed in), and onto
 * another installed distributor when the one in use is gone. A registration the app removed itself
 * (sign-out, switching distributors) is never brought back.
 */
object UnifiedPushRecovery {
  private const val TAG = "TeamCraftPush"
  private const val WORK_NAME = "unifiedpush-reregister"
  private const val FILE = "unifiedpush_recovery"
  private const val KEY_IS_WANTED = "is_wanted"
  private const val KEY_ATTEMPTS = "attempts"
  private const val KEY_SELF_UNREGISTERED_AT = "self_unregistered_at"

  /** First wait; it doubles per attempt up to [MAX_DELAY_MS]. */
  private const val INITIAL_DELAY_MS = 30_000L
  private const val MAX_DELAY_MS = 6 * 60 * 60 * 1000L

  /** Attempts in a row before the app waits for the user (the diagnostics offer registering). */
  const val MAX_ATTEMPTS = 12

  /**
   * How long after the app unregistered itself an "unregistered" from the distributor is taken as
   * the answer to it (the distributor answers asynchronously).
   */
  private const val SELF_UNREGISTER_WINDOW_MS = 60_000L

  /** Failure reasons of the connector that another try can't fix (the user or server must act). */
  private val PERMANENT_REASONS = setOf("ACTION_REQUIRED", "VAPID_REQUIRED")

  /** Push is wanted from now on (the app registered). */
  fun onRegisterRequested(context: Context) {
    prefs(context).edit().putBoolean(KEY_IS_WANTED, true).apply()
  }

  /** The app removes its registration itself: sign-out (push no longer wanted) or a switch. */
  fun onSelfUnregister(context: Context, isStillWanted: Boolean) {
    prefs(context).edit()
      .putBoolean(KEY_IS_WANTED, isStillWanted)
      .putLong(KEY_SELF_UNREGISTERED_AT, System.currentTimeMillis())
      .apply()
    if (!isStillWanted) cancel(context)
  }

  /** A new endpoint arrived: the registration works again. */
  fun onRegistered(context: Context) {
    prefs(context).edit().putInt(KEY_ATTEMPTS, 0).apply()
    cancel(context)
  }

  fun onRegistrationFailed(context: Context, reasonName: String) {
    if (reasonName in PERMANENT_REASONS) {
      Log.i(TAG, "UnifiedPush registration needs the user: $reasonName")
      return
    }
    schedule(context)
  }

  fun onUnregistered(context: Context) {
    val prefs = prefs(context)
    val sinceSelf = System.currentTimeMillis() - prefs.getLong(KEY_SELF_UNREGISTERED_AT, 0)
    if (sinceSelf in 0..SELF_UNREGISTER_WINDOW_MS) return
    schedule(context)
  }

  fun onTemporarilyUnavailable(context: Context) = schedule(context)

  /** The wait before attempt [attempt] (1-based). */
  fun delayMs(attempt: Int): Long {
    val exponent = (attempt - 1).coerceIn(0, 20)
    return (INITIAL_DELAY_MS shl exponent).coerceAtMost(MAX_DELAY_MS)
  }

  private fun schedule(context: Context) {
    val prefs = prefs(context)
    if (!prefs.getBoolean(KEY_IS_WANTED, false)) return
    val attempt = prefs.getInt(KEY_ATTEMPTS, 0) + 1
    if (attempt > MAX_ATTEMPTS) {
      Log.i(TAG, "UnifiedPush re-registration gave up after $MAX_ATTEMPTS attempts")
      return
    }
    prefs.edit().putInt(KEY_ATTEMPTS, attempt).apply()
    val request = OneTimeWorkRequestBuilder<Worker>()
      .setInitialDelay(delayMs(attempt), TimeUnit.MILLISECONDS)
      .setConstraints(Constraints.Builder().setRequiredNetworkType(NetworkType.CONNECTED).build())
      .build()
    // The newest signal decides the wait; a pending attempt is replaced, never doubled.
    WorkManager.getInstance(context).enqueueUniqueWork(WORK_NAME, ExistingWorkPolicy.REPLACE, request)
  }

  private fun cancel(context: Context) {
    WorkManager.getInstance(context).cancelUniqueWork(WORK_NAME)
  }

  private fun prefs(context: Context) = context.getSharedPreferences(FILE, Context.MODE_PRIVATE)

  /** One re-registration attempt; its outcome arrives at [UnifiedPushReceiver] like any other. */
  class Worker(context: Context, parameters: WorkerParameters) : CoroutineWorker(context, parameters) {
    override suspend fun doWork(): Result {
      val context = applicationContext
      if (!prefs(context).getBoolean(KEY_IS_WANTED, false)) return Result.success()
      val installed = UnifiedPush.getDistributors(context)
      if (installed.isEmpty()) return Result.success()
      // The distributor in use was uninstalled: take another one the user has.
      val maybeCurrent = UnifiedPush.getAckDistributor(context) ?: UnifiedPush.getSavedDistributor(context)
      if (maybeCurrent == null || maybeCurrent !in installed) {
        UnifiedPush.saveDistributor(context, installed.first())
      }
      UnifiedPush.register(context)
      return Result.success()
    }
  }
}
