package de.zeitgleich.teamcraft.core

import android.os.Looper
import de.zeitgleich.teamcraft.BuildConfig
import de.zeitgleich.teamcraft.crash.CrashSdks

/**
 * The main thread never waits (#2184): a call that can block (the token bridge, the HTTP bridge,
 * both `runBlocking`) checks first that it isn't on the main thread. Debug builds throw there, so
 * the mistake shows the moment it is made; release builds report each call site once to the crash
 * reports, by name only (docs/crash-reports.md: no content), and go on.
 *
 * Core calls that can block are `suspend` exports and can't be called and waited for on the main
 * thread; this covers what the app itself does that can block. `StrictMode` catches disk and
 * network on the main thread in debug builds (`TeamCraftApplication`).
 */
class UiThreadGuard(
  private val isOnUiThread: () -> Boolean = { Looper.getMainLooper().isCurrentThread },
  private val failsLoudly: Boolean = BuildConfig.DEBUG,
  private val report: (String) -> Unit = { CrashSdks.platform().captureWarning("ui-thread-wait", it) },
) {
  private val reported = mutableSetOf<String>()

  /** Call first in anything that may block. [callSite] names the function, never what it works on. */
  fun check(callSite: String) {
    if (!isOnUiThread()) return
    val isFirst = synchronized(reported) { reported.add(callSite) }
    if (failsLoudly) throw IllegalStateException("$callSite can block and ran on the main thread (#2184)")
    if (isFirst) report(callSite)
  }

  companion object {
    val shared = UiThreadGuard()
  }
}
