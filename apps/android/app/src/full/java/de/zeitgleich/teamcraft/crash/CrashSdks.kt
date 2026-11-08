package de.zeitgleich.teamcraft.crash

import android.content.Context
import android.util.Log
import io.sentry.KeyValueCollectionBehavior
import io.sentry.Sentry
import io.sentry.ITransaction
import io.sentry.SentryOptions
import io.sentry.SpanStatus
import io.sentry.TransactionOptions
import io.sentry.android.core.SentryAndroid
import io.sentry.android.core.SentryAndroidOptions
import io.sentry.protocol.User
import kotlin.random.Random
import uniffi.teamcraft_ffi.recentLogLines

/** Google Play builds: sentry-android (JVM, NDK and ANR crashes). */
object CrashSdks {
  fun platform(): CrashSdk = SentryCrashSdk
}

/**
 * sentry-android, started by hand (auto-init is off in the manifest) and set up so it collects only
 * what docs/crash-reports.md allows: no PII, screenshots, view hierarchy, replay or automatic
 * breadcrumbs; every event passes [SentryEventScrub] before it is sent. Performance data (app start,
 * screen loads, slow and frozen frames, the core's measures from [Perf]) is traced at the
 * instance's trace rate and passes [SentryTransactionScrub].
 */
private object SentryCrashSdk : CrashSdk {
  private const val TAG = "CrashReports"
  private var maybeRunning: CrashSdkSettings? = null

  override val isAvailable = true

  @Synchronized
  override fun start(context: Context, settings: CrashSdkSettings) {
    if (maybeRunning == settings) return
    if (maybeRunning != null) Sentry.close()
    try {
      SentryAndroid.init(context.applicationContext) { options -> configure(options, settings) }
      // The scope's user too: NDK crashes are written by the native SDK from the synced scope.
      Sentry.setUser(User().apply { id = settings.installationId })
      maybeRunning = settings
    } catch (failure: RuntimeException) {
      // A bad DSN or an SDK failure must never take the app down; reports just stay off.
      Log.w(TAG, "crash reports not started: ${failure.javaClass.simpleName}")
      maybeRunning = null
    }
  }

  @Synchronized
  override fun stop() {
    if (maybeRunning == null) return
    Sentry.close()
    maybeRunning = null
  }

  override fun startTransaction(name: String, op: String): PerfHandle? {
    if (maybeRunning == null) return null
    val options = TransactionOptions().apply { isBindToScope = false }
    val transaction = runCatching { Sentry.startTransaction(name, op, options) }.getOrNull() ?: return null
    // Left out by the trace rate: nothing to finish or send.
    if (transaction.isSampled != true) return null
    return SentryPerfHandle(transaction)
  }

  private fun configure(options: SentryAndroidOptions, settings: CrashSdkSettings) {
    options.dsn = settings.config.dsn
    options.release = settings.release
    options.environment = settings.config.environment
    // Sampling of handled errors happens in beforeSend: the SDK's own rate would sample crashes too.
    options.sampleRate = null
    // No personal data at all (what `sendDefaultPii = false` was; that option is deprecated).
    options.dataCollection.apply {
      userInfo = false
      cookies = KeyValueCollectionBehavior.off()
      urlQueryParams = KeyValueCollectionBehavior.off()
      httpHeaders.request = KeyValueCollectionBehavior.off()
      httpHeaders.response = KeyValueCollectionBehavior.off()
      httpBodies = emptySet()
      databaseQueryData = false
      filePaths = false
    }
    options.isAttachScreenshot = false
    options.isAttachViewHierarchy = false
    options.isAttachThreads = false
    // Performance data (issue #1521): off at a rate of 0, as from an older server.
    options.tracesSampleRate = settings.config.traceRate.takeIf { it > 0.0 }
    options.profilesSampleRate = null
    options.sessionReplay.sessionSampleRate = null
    options.sessionReplay.onErrorSampleRate = null
    // No sessions: release health isn't used, and an app that doesn't crash sends nothing.
    options.isEnableAutoSessionTracking = false
    options.isEnableNdk = true
    options.isAnrEnabled = true
    options.isEnableUserInteractionBreadcrumbs = false
    options.isEnableActivityLifecycleBreadcrumbs = false
    options.isEnableAppLifecycleBreadcrumbs = false
    options.isEnableSystemEventBreadcrumbs = false
    options.isEnableAppComponentBreadcrumbs = false
    options.isEnableNetworkEventBreadcrumbs = false
    options.isEnableUserInteractionTracing = false
    // Screen loads (`ui.load`, time to initial display) and the app start; frames are on by default.
    options.isEnableAutoActivityLifecycleTracing = true
    options.isEnableTimeToFullDisplayTracing = false
    options.isEnableFramesTracking = true
    // App start from the process start, with its phases.
    options.isEnablePerformanceV2 = true
    // Breadcrumbs come from the core's redacted log only, set in beforeSend.
    options.beforeBreadcrumb = SentryOptions.BeforeBreadcrumbCallback { _, _ -> null }
    options.beforeSendTransaction = SentryOptions.BeforeSendTransactionCallback { transaction, _ ->
      if (transaction.getTag(SentryPerfHandle.DROPPED_TAG) != null) {
        null
      } else {
        SentryTransactionScrub.prepare(transaction, installationId = settings.installationId)
      }
    }
    options.beforeSend = SentryOptions.BeforeSendCallback { event, _ ->
      SentryEventScrub.prepare(
        event,
        installationId = settings.installationId,
        sampleRate = settings.config.sampleRate,
        roll = Random.nextDouble(),
        logLines = { runCatching { recentLogLines() }.getOrDefault(emptyList()) },
      )
    }
  }
}

/** A measure of [Perf] as a sentry transaction. */
private class SentryPerfHandle(private val transaction: ITransaction) : PerfHandle {
  override fun tag(name: String, value: String) = transaction.setTag(name, value)

  override fun finish(isOk: Boolean) = transaction.finish(if (isOk) SpanStatus.OK else SpanStatus.UNAVAILABLE)

  override fun cancel() {
    // The SDK has no way to drop a started transaction: `beforeSendTransaction` drops it by this tag.
    transaction.setTag(DROPPED_TAG, "1")
    transaction.finish(SpanStatus.CANCELLED)
  }

  companion object {
    const val DROPPED_TAG = "teamcraft.dropped"
  }
}
