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
   * core's rows, tagged cold or warm and by [entry] (launcher, notification, share, link), with the
   * copies of the activity alive as a measurement.
   */
  fun beginStart(startedAtMs: Long, isCold: Boolean, entry: String, activities: Int) {
    begin(PerfMeasure.START_TO_CHANNEL_LIST, NO_KEY, startedAtMs)
    tag(PerfMeasure.START_TO_CHANNEL_LIST, NO_KEY, "start", if (isCold) "cold" else "warm")
    tag(PerfMeasure.START_TO_CHANNEL_LIST, NO_KEY, "entry", entry)
    measure(PerfMeasure.START_TO_CHANNEL_LIST, NO_KEY, "activities", activities.toDouble())
  }

  /** The first frame was drawn: a step of the start, and whether it showed the cached chat list. */
  @Synchronized
  fun firstFrame(startedAtMs: Long, isCachedChatList: Boolean) {
    val handle = pending[PerfMeasure.START_TO_CHANNEL_LIST to NO_KEY] ?: return
    handle.step("ui.render", "first_frame", startedAtMs, System.currentTimeMillis())
    handle.tag("start_cache", if (isCachedChatList) "hit" else "miss")
  }

  /** The chat list shows rows read from the core: the start is done (once per start). */
  fun chatListShown() = end(PerfMeasure.START_TO_CHANNEL_LIST)

  /** The chat list couldn't be read from the core. */
  fun chatListFailed() = end(PerfMeasure.START_TO_CHANNEL_LIST, isOk = false)

  @Synchronized
  fun measure(measure: PerfMeasure, key: String, name: String, value: Double) {
    pending[measure to key]?.measure(name, value)
  }

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
