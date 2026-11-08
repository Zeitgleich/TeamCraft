package de.zeitgleich.teamcraft.crash

import android.content.Context
import android.util.Log
import io.sentry.KeyValueCollectionBehavior
import io.sentry.Sentry
import io.sentry.SentryOptions
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
 * what docs/crash-reports.md allows: no PII, screenshots, view hierarchy, replay, tracing or
 * automatic breadcrumbs; every event passes [SentryEventScrub] before it is sent.
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
    options.tracesSampleRate = null
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
    options.isEnableAutoActivityLifecycleTracing = false
    // Breadcrumbs come from the core's redacted log only, set in beforeSend.
    options.beforeBreadcrumb = SentryOptions.BeforeBreadcrumbCallback { _, _ -> null }
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
