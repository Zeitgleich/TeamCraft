package de.zeitgleich.teamcraft.crash

import android.content.Context

/**
 * The platform crash SDK (docs/crash-reports.md, "The Android SDK"): sentry-android in the `full`
 * flavor, nothing in `libre` ([CrashSdks]). [CrashReports] decides when it runs.
 */
interface CrashSdk {
  /** Whether this build can send crash reports at all; the settings show the switch only then. */
  val isAvailable: Boolean

  /** Starts capturing with [settings], or restarts with them when they changed. */
  fun start(context: Context, settings: CrashSdkSettings)

  /** Stops capturing and sending; reports not sent yet stay on the device until the next start. */
  fun stop()
}

/** What the SDK is started with: the instance's setting, this build's release and the installation. */
data class CrashSdkSettings(
  val config: CrashConfig,
  /** `teamcraft@<version>`, as the other apps report it. */
  val release: String,
  /** The random installation ID, the report's only `user` field. */
  val installationId: String,
)

/** A build without a crash SDK (`libre`): never captures, never sends. */
object NoCrashSdk : CrashSdk {
  override val isAvailable = false

  override fun start(context: Context, settings: CrashSdkSettings) = Unit

  override fun stop() = Unit
}
