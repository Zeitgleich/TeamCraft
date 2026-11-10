package de.zeitgleich.teamcraft.crash

import uniffi.teamcraft_ffi.ConnectionState
import uniffi.teamcraft_ffi.PerfMeasure
import uniffi.teamcraft_ffi.perfMeasureName

/**
 * Performance data of the app's own operations (issue #1521, docs/crash-reports.md "Performance
 * data"): a channel switch, a send until the server took it, a reconnect. Named as on desktop and
 * web (`perfMeasureName` from the core), sent by the crash SDK at the instance's trace rate, which
 * also decides whether a measure runs at all; the app start and screen loads are the SDK's own.
 *
 * A measure that starts in one place and ends in another is kept by a local key (a conversation's
 * ID), which never leaves the device. Starting it again for the same key drops the running one.
 */
object Perf {
  /** Measures that may run side by side at most; a further one drops the oldest. */
  private const val MAX_PENDING = 16
  private const val NO_KEY = ""

  @Volatile private var sdk: CrashSdk = NoCrashSdk
  private val pending = LinkedHashMap<Pair<PerfMeasure, String>, PerfHandle>()

  /** When the running start's activity was created: its first frame is judged from there. */
  private var startCreatedAtMs = 0L
  private var isStartFrameDrawn = false
  /** The chat list's outcome when it came before the first frame: the start ends with that frame. */
  private var maybeStartOutcome: StartOutcome? = null

  /** The crash SDK of this build, set once at start. */
  fun install(crashSdk: CrashSdk) {
    sdk = crashSdk
  }

  @Synchronized
  fun begin(measure: PerfMeasure, key: String = NO_KEY, startedAtMs: Long = System.currentTimeMillis()) {
    pending.remove(measure to key)?.cancel()
    val name = runCatching { perfMeasureName(measure) }.getOrNull() ?: return
    val handle = sdk.startTransaction(name.name, name.op, startedAtMs) ?: return
    if (pending.size >= MAX_PENDING) {
      pending.keys.firstOrNull()?.let { pending.remove(it)?.cancel() }
    }
    pending[measure to key] = handle
  }

  @Synchronized
  fun isRunning(measure: PerfMeasure, key: String = NO_KEY): Boolean = (measure to key) in pending

  @Synchronized
  fun tag(measure: PerfMeasure, key: String, name: String, value: String) {
    pending[measure to key]?.tag(name, value)
  }

  @Synchronized
  fun end(measure: PerfMeasure, key: String = NO_KEY, isOk: Boolean = true) {
    pending.remove(measure to key)?.finish(isOk)
  }

  @Synchronized
  fun cancel(measure: PerfMeasure, key: String = NO_KEY) {
    pending.remove(measure to key)?.cancel()
  }

  /**
   * The signed-in start (`start.channel_list`): from [startedAtMs] until the chat list shows the
   * core's rows on a drawn frame, tagged cold or warm and by [entry] (launcher, notification, share,
   * link), with the copies of the activity alive as a measurement. [createdAtMs] is when the
   * activity was created: a first frame long after it means nobody saw the start (#1641).
   */
  @Synchronized
  fun beginStart(startedAtMs: Long, createdAtMs: Long, isCold: Boolean, entry: String, activities: Int) {
    startCreatedAtMs = createdAtMs
    isStartFrameDrawn = false
    maybeStartOutcome = null
    begin(PerfMeasure.START_TO_CHANNEL_LIST, NO_KEY, startedAtMs)
    tag(PerfMeasure.START_TO_CHANNEL_LIST, NO_KEY, "start", if (isCold) "cold" else "warm")
    tag(PerfMeasure.START_TO_CHANNEL_LIST, NO_KEY, "entry", entry)
    measure(PerfMeasure.START_TO_CHANNEL_LIST, NO_KEY, "activities", activities.toDouble())
  }

  /**
   * The first frame was drawn: a step of the start, and whether it showed the cached chat list. A
   * first frame that came long after the activity was created (screen off, locked, started in the
   * background) ends a start nobody watched: it is dropped, not sent as slow (#1641).
   */
  @Synchronized
  fun firstFrame(startedAtMs: Long, isCachedChatList: Boolean) {
    val handle = pending[PerfMeasure.START_TO_CHANNEL_LIST to NO_KEY] ?: return
    val nowMs = System.currentTimeMillis()
    if (!StartVisibility.isSeen(startCreatedAtMs, nowMs)) {
      cancel(PerfMeasure.START_TO_CHANNEL_LIST)
      return
    }
    handle.step("ui.render", "first_frame", startedAtMs, nowMs)
    handle.tag("start_cache", if (isCachedChatList) "hit" else "miss")
    isStartFrameDrawn = true
    maybeStartOutcome?.let { endStart(it) }
  }

  /** The chat list shows rows read from the core: the start is done once a frame shows them. */
  fun chatListShown() = startOutcome(StartOutcome.SHOWN)

  /** The chat list couldn't be read from the core. */
  fun chatListFailed() = startOutcome(StartOutcome.FAILED)

  @Synchronized
  fun measure(measure: PerfMeasure, key: String, name: String, value: Double) {
    pending[measure to key]?.measure(name, value)
  }

  @Synchronized
  private fun startOutcome(outcome: StartOutcome) {
    if (!isRunning(PerfMeasure.START_TO_CHANNEL_LIST)) return
    // Read before the first frame (the activity not visible yet): the frame decides.
    if (!isStartFrameDrawn) {
      if (maybeStartOutcome == null) maybeStartOutcome = outcome
      return
    }
    endStart(outcome)
  }

  private fun endStart(outcome: StartOutcome) = end(PerfMeasure.START_TO_CHANNEL_LIST, isOk = outcome == StartOutcome.SHOWN)

  /** The core's connection changed: a reconnect runs from connecting again until connected. */
  fun connectionChanged(previous: ConnectionState?, current: ConnectionState?) {
    when (current) {
      is ConnectionState.Connecting -> if (previous != null && previous !is ConnectionState.Connecting) begin(PerfMeasure.RESYNC)
      is ConnectionState.Connected -> end(PerfMeasure.RESYNC)
      is ConnectionState.Slow -> {
        tag(PerfMeasure.RESYNC, NO_KEY, "link", "slow")
        end(PerfMeasure.RESYNC)
      }
      is ConnectionState.Offline -> end(PerfMeasure.RESYNC, isOk = false)
      else -> cancel(PerfMeasure.RESYNC)
    }
  }
}

/**
 * Whether the user saw a start or a screen load (#1641, docs/crash-reports.md "Performance data").
 * An activity created while not visible (screen off, locked, restored or started in the background)
 * draws its first frame only once it is shown, minutes or hours later; timed, that is not a slow
 * start but one nobody waited for. Likewise a process started for a push and opened later is not a
 * cold start from the process start.
 */
object StartVisibility {
  /** A first frame later than this after the activity's creation was not watched. */
  const val FIRST_FRAME_SEEN_WITHIN_MS = 10_000L

  /** The first activity created later than this after the process start makes a warm start. */
  const val COLD_START_WITHIN_MS = 30_000L

  /** Whether a first frame at [firstFrameAtMs] of an activity created at [createdAtMs] was seen. */
  fun isSeen(createdAtMs: Long, firstFrameAtMs: Long): Boolean = firstFrameAtMs - createdAtMs <= FIRST_FRAME_SEEN_WITHIN_MS

  /** Whether the process's first activity, created at [createdAtMs], started cold. */
  fun isColdStart(processStartedAtMs: Long, createdAtMs: Long): Boolean = createdAtMs - processStartedAtMs <= COLD_START_WITHIN_MS
}

/** How the start's chat list read ended. */
private enum class StartOutcome {
  SHOWN,
  FAILED,
}

/**
 * A conversation's catch-up (`conversation.catch_up`, issue #1521): from opening it, or the app
 * coming back to the foreground, until the newest server message is in the store and shown, so
 * "the push came, the chat lacks it" shows even when the switch was fast from the seed. Only runs
 * while the conversation is behind (a notified message the store lacks, or a catch-up running).
 */
class CatchUpMeasure(private val conversationId: String, private val openedAtMs: Long) {
  private var isRunning = false
  private var isFirstLook = true
  private var maybeResumedAtMs: Long? = null

  /** The app came back to the foreground with this conversation open. */
  fun resumed() {
    if (!isFirstLook) maybeResumedAtMs = System.currentTimeMillis()
  }

  /** What the latest read found: [isBehind] while the store lacks what the server has. */
  fun observe(isBehind: Boolean, isFromPush: Boolean, hasGaps: Boolean) {
    val maybeResumedAt = maybeResumedAtMs
    if (isBehind && !isRunning) {
      val (startedAt, source) = when {
        isFirstLook && isFromPush -> openedAtMs to "push"
        isFirstLook -> openedAtMs to "open"
        maybeResumedAt != null -> maybeResumedAt to "foreground"
        else -> System.currentTimeMillis() to "open"
      }
      Perf.begin(PerfMeasure.CATCH_UP, conversationId, startedAt)
      Perf.tag(PerfMeasure.CATCH_UP, conversationId, "source", source)
      if (Perf.isRunning(PerfMeasure.RESYNC)) Perf.tag(PerfMeasure.CATCH_UP, conversationId, "resync", "yes")
      isRunning = true
    }
    if (isRunning && hasGaps) Perf.tag(PerfMeasure.CATCH_UP, conversationId, "gap_fill", "yes")
    if (isRunning && !isBehind) {
      Perf.end(PerfMeasure.CATCH_UP, conversationId)
      isRunning = false
    }
    isFirstLook = false
    maybeResumedAtMs = null
  }

  /** The screen went away: a catch-up the user didn't wait for says nothing. */
  fun dispose() = Perf.cancel(PerfMeasure.CATCH_UP, conversationId)
}

/** A running measure of the crash SDK. */
interface PerfHandle {
  /** A tag from the code (`source` = `seed`), never content. */
  fun tag(name: String, value: String)

  /** A number from the code (`activities`), never content. */
  fun measure(name: String, value: Double)

  /** A step of the app's own between two Unix milliseconds (`ui.render` / `first_frame`). */
  fun step(op: String, description: String, startMs: Long, endMs: Long)

  /** Ends it now, with the core's jobs that overlapped it as its steps (`perfJobSpans`). */
  fun finish(isOk: Boolean)

  /** Dropped unsent: the user moved on before it ended. */
  fun cancel()
}
