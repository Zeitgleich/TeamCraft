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
  fun begin(measure: PerfMeasure, key: String = NO_KEY) {
    pending.remove(measure to key)?.cancel()
    val name = runCatching { perfMeasureName(measure) }.getOrNull() ?: return
    val handle = sdk.startTransaction(name.name, name.op) ?: return
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

/** A running measure of the crash SDK. */
interface PerfHandle {
  /** A tag from the code (`source` = `seed`), never content. */
  fun tag(name: String, value: String)

  fun finish(isOk: Boolean)

  /** Dropped unsent: the user moved on before it ended. */
  fun cancel()
}
