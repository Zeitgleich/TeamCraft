package de.zeitgleich.teamcraft.crash

import android.content.Context
import android.os.Build
import android.util.Log
import io.sentry.android.core.InternalSentrySdk
import java.util.concurrent.Executors
import io.sentry.KeyValueCollectionBehavior
import io.sentry.Sentry
import io.sentry.ITransaction
import io.sentry.SentryOptions
import io.sentry.SpanStatus
import io.sentry.TransactionOptions
import io.sentry.android.core.SentryAndroid
import io.sentry.android.core.SentryAndroidOptions
import io.sentry.protocol.User
import io.sentry.Instrumenter
import io.sentry.SentryDate
import io.sentry.SentryEvent
import io.sentry.SentryLevel
import io.sentry.SentryLongDate
import io.sentry.protocol.Message
import kotlin.random.Random
import uniffi.teamcraft_ffi.PerfJobKind
import uniffi.teamcraft_ffi.PerfJobSpan
import uniffi.teamcraft_ffi.PerfListener
import uniffi.teamcraft_ffi.perfJobSpans
import uniffi.teamcraft_ffi.recentLogLines
import uniffi.teamcraft_ffi.setPerfListener
import uniffi.teamcraft_ffi.setSlowJobThresholdMs
import uniffi.teamcraft_ffi.takeTraceEnvelopes
import uniffi.teamcraft_ffi.traceStart
import uniffi.teamcraft_ffi.traceStop

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
      // The core's slow jobs as sampled warnings (issue #1521), with the instance's threshold.
      setSlowJobThresholdMs(settings.config.slowJobMs.toULong())
      setPerfListener(SlowJobReports)
      // The core's spans (docs/tracing.md): slow, failed and sampled traces as transactions.
      traceStart(
        traceRate = settings.config.traceRate,
        slowJobMs = settings.config.slowJobMs.toULong(),
        environment = settings.config.environment,
        appVersion = settings.release.removePrefix("teamcraft@"),
        installationId = settings.installationId,
        osVersion = Build.VERSION.RELEASE ?: "",
      )
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
    setSlowJobThresholdMs(0uL)
    traceStop()
    Sentry.close()
    maybeRunning = null
  }

  override fun startTransaction(name: String, op: String, startedAtMs: Long): PerfHandle? {
    if (maybeRunning == null) return null
    val options = TransactionOptions().apply {
      isBindToScope = false
      startTimestamp = sentryDate(startedAtMs.toDouble())
    }
    val transaction = runCatching { Sentry.startTransaction(name, op, options) }.getOrNull() ?: return null
    // Left out by the trace rate: nothing to finish or send.
    if (transaction.isSampled != true) return null
    return SentryPerfHandle(transaction)
  }

  override fun captureHandled(what: String, cause: String) {
    if (maybeRunning == null) return
    val event = SentryEvent().apply {
      level = SentryLevel.ERROR
      message = Message().apply { formatted = "Handled: $what" }
      fingerprints = listOf("handled", what, cause)
      setTag("cause", cause)
    }
    runCatching { Sentry.captureEvent(event) }
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
      val isDropped = transaction.getTag(SentryPerfHandle.DROPPED_TAG) != null
      if (isDropped || SentryTransactionScrub.isUnseenScreenLoad(transaction)) {
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

/** A measure of [Perf] as a sentry transaction, with the core's jobs as its steps. */
private class SentryPerfHandle(private val transaction: ITransaction) : PerfHandle {
  override fun tag(name: String, value: String) = transaction.setTag(name, value)

  override fun measure(name: String, value: Double) = transaction.setMeasurement(name, value)

  override fun step(op: String, description: String, startMs: Long, endMs: Long) {
    val step = transaction.startChild(op, description, sentryDate(startMs.toDouble()), Instrumenter.SENTRY)
    step.finish(SpanStatus.OK, sentryDate(endMs.toDouble()))
  }

  override fun finish(isOk: Boolean) {
    val endMs = System.currentTimeMillis().toDouble()
    val startMs = transaction.startDate.nanoTimestamp() / 1_000_000.0
    runCatching { perfJobSpans(startMs, endMs) }.getOrDefault(emptyList()).forEach(::addJob)
    transaction.finish(if (isOk) SpanStatus.OK else SpanStatus.UNAVAILABLE, sentryDate(endMs))
  }

  override fun cancel() {
    // The SDK has no way to drop a started transaction: `beforeSendTransaction` drops it by this tag.
    transaction.setTag(DROPPED_TAG, "1")
    transaction.finish(SpanStatus.CANCELLED)
  }

  /**
   * A core job as two steps: its wait in the queue (`queue.wait`) and its run (`core.read`,
   * `core.engine`, `store.lock_wait`, `store.write_group`, `core.step`), named by the job's
   * function or the step's name in the code.
   */
  private fun addJob(job: PerfJobSpan) {
    // A span of the core's trace layer names its own operation (`http.client`); a queue job doesn't.
    val op = job.op.ifEmpty { jobOp(job.kind) }
    if (job.startedAtMs - job.enqueuedAtMs >= MIN_STEP_MS) {
      val wait = transaction.startChild("queue.wait", job.name, sentryDate(job.enqueuedAtMs), Instrumenter.SENTRY)
      wait.setData(QUEUE_DEPTH_DATA, job.queueDepth.toLong())
      wait.setData(QUEUE_DATA, op)
      wait.finish(SpanStatus.OK, sentryDate(job.startedAtMs))
    }
    val run = transaction.startChild(op, job.name, sentryDate(job.startedAtMs), Instrumenter.SENTRY)
    run.setData(QUEUE_DEPTH_DATA, job.queueDepth.toLong())
    run.finish(SpanStatus.OK, sentryDate(job.doneAtMs))
  }

  companion object {
    const val DROPPED_TAG = "teamcraft.dropped"
    const val QUEUE_DEPTH_DATA = "teamcraft.queue_depth"
    const val QUEUE_DATA = "teamcraft.queue"
    private const val MIN_STEP_MS = 1.0
  }
}

/**
 * A core job that waited or ran longer than the instance's threshold, as a warning (sampled with
 * the handled errors' rate in `beforeSend`), grouped per job: its name, queue, wait, run, the
 * thread's CPU time while it ran and the queue's depth as tags. Names are functions in the code
 * (and query kinds), never data.
 */
private object SlowJobReports : PerfListener {
  override fun onSlowJob(job: PerfJobSpan) {
    val waitMs = (job.startedAtMs - job.enqueuedAtMs).toLong()
    val runMs = (job.doneAtMs - job.startedAtMs).toLong()
    val event = SentryEvent().apply {
      level = SentryLevel.WARNING
      message = Message().apply { formatted = "Slow core job: ${job.name}" }
      fingerprints = listOf("slow-core-job", job.name)
      setTag("job", job.name)
      setTag("queue", jobOp(job.kind))
      setTag("wait_ms", waitMs.toString())
      setTag("run_ms", runMs.toString())
      setTag("queue_depth", job.queueDepth.toString())
      // Far below run_ms: the job waited (a lock, or the process frozen in the background).
      job.maybeCpuMs?.let { setTag("cpu_ms", it.toLong().toString()) }
    }
    runCatching { Sentry.captureEvent(event) }
  }

  /** One thread takes the kept traces off the core's thread and hands them to the SDK's queue. */
  private val traces = Executors.newSingleThreadExecutor { runnable -> Thread(runnable, "teamcraft-traces") }

  /**
   * Kept traces (docs/tracing.md) are envelopes the core built and scrubbed; the SDK sends them as
   * they are, with its own queue, retries and rate limits.
   */
  override fun onTracesKept() {
    traces.execute {
      runCatching { takeTraceEnvelopes() }.getOrDefault(emptyList()).forEach { envelope ->
        // A refused envelope (SDK closed meanwhile) is only a trace not sent.
        runCatching { InternalSentrySdk.captureEnvelope(envelope, false) }
      }
    }
  }
}

private fun jobOp(kind: PerfJobKind): String = when (kind) {
  PerfJobKind.READ_QUEUE -> "core.read"
  PerfJobKind.ENGINE_QUEUE -> "core.engine"
  PerfJobKind.STORE_LOCK -> "store.lock_wait"
  PerfJobKind.WRITE_GROUP -> "store.write_group"
  PerfJobKind.CORE_STEP -> "core.step"
}

/** Unix milliseconds (with a fraction) as the SDK's date. */
private fun sentryDate(unixMs: Double): SentryDate = SentryLongDate((unixMs * 1_000_000.0).toLong())
