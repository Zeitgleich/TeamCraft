package de.zeitgleich.teamcraft.search

import android.content.Context
import android.util.Log
import androidx.work.Constraints
import androidx.work.CoroutineWorker
import androidx.work.ExistingPeriodicWorkPolicy
import androidx.work.NetworkType
import androidx.work.PeriodicWorkRequestBuilder
import androidx.work.WorkManager
import androidx.work.WorkerParameters
import java.util.concurrent.TimeUnit
import de.zeitgleich.teamcraft.TeamCraftApplication
import uniffi.teamcraft_ffi.AccountState

/**
 * Downloads the shared search segments of older months (e2ee.md §10.1) so history the phone
 * doesn't keep stays searchable: only on unmetered networks while charging, up to a size cap the
 * user chooses. The core picks the segments (newest first) and enforces the cap.
 */
class SegmentPrefetch(private val context: Context) {
  private val prefs = context.getSharedPreferences(FILE, Context.MODE_PRIVATE)

  val isEnabled: Boolean get() = prefs.getBoolean(KEY_ENABLED, true)

  val maxMegabytes: Int get() = prefs.getInt(KEY_MAX_MB, DEFAULT_MAX_MB)

  fun update(isEnabled: Boolean, maxMegabytes: Int) {
    prefs.edit().putBoolean(KEY_ENABLED, isEnabled).putInt(KEY_MAX_MB, maxMegabytes).apply()
    schedule()
  }

  /** (Re)plans the periodic prefetch, or cancels it when turned off. */
  fun schedule() {
    val work = WorkManager.getInstance(context)
    if (!isEnabled) {
      work.cancelUniqueWork(WORK_NAME)
      return
    }
    val constraints = Constraints.Builder()
      .setRequiredNetworkType(NetworkType.UNMETERED)
      .setRequiresCharging(true)
      .build()
    val request = PeriodicWorkRequestBuilder<Worker>(INTERVAL_HOURS, TimeUnit.HOURS)
      .setConstraints(constraints)
      .build()
    work.enqueueUniquePeriodicWork(WORK_NAME, ExistingPeriodicWorkPolicy.UPDATE, request)
  }

  /** One prefetch run. */
  class Worker(context: Context, parameters: WorkerParameters) : CoroutineWorker(context, parameters) {
    override suspend fun doWork(): Result {
      val graph = TeamCraftApplication.graph(applicationContext)
      val settings = graph.segmentPrefetch
      if (!settings.isEnabled || !graph.auth.isSignedIn.value) return Result.success()
      return try {
        val core = graph.core.core()
        if (core.accountState() !is AccountState.Ready) return Result.success()
        val result = core.prefetchSearchSegments(settings.maxMegabytes.toULong() * BYTES_PER_MEGABYTE)
        Log.i(TAG, "search segments prefetched: ${result.fetched}, skipped for size: ${result.skipped}")
        Result.success()
      } catch (error: Exception) {
        Log.w(TAG, "search segment prefetch failed: ${error.javaClass.simpleName}")
        Result.retry()
      }
    }
  }

  companion object {
    private const val TAG = "TeamCraftSearch"
    private const val FILE = "segment_prefetch"
    private const val KEY_ENABLED = "enabled"
    private const val KEY_MAX_MB = "max_mb"
    private const val WORK_NAME = "segment_prefetch"
    private const val INTERVAL_HOURS = 24L
    private const val BYTES_PER_MEGABYTE = 1_048_576UL
    const val DEFAULT_MAX_MB = 500
    /** The caps offered in settings. */
    val CHOICES_MB = listOf(100, 250, 500, 1000, 2000)
  }
}
