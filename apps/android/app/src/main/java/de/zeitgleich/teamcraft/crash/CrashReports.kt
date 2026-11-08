package de.zeitgleich.teamcraft.crash

import android.content.Context
import android.content.SharedPreferences
import android.os.SystemClock
import java.io.IOException
import java.security.SecureRandom
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import de.zeitgleich.teamcraft.BuildConfig
import de.zeitgleich.teamcraft.server.ServerInfoClient
import uniffi.teamcraft_ffi.StoredServer
import uniffi.teamcraft_ffi.crashReportsConfig

/**
 * Crash reports of the app (docs/crash-reports.md): runs the platform SDK ([CrashSdk]) only while
 * the signed-in server has crash reports on (its `server-info`) and the user didn't turn them off,
 * or the instance makes them mandatory. The last known setting is kept, so a crash early in the
 * next start is reported too. Everything lives in app-private preferences, never backed up.
 */
class CrashReports(private val context: Context, private val sdk: CrashSdk) {
  private val prefs: SharedPreferences = context.getSharedPreferences(FILE, Context.MODE_PRIVATE)
  private val statusFlow = MutableStateFlow(status(maybeConfig()))

  /** Reports are on for this instance and this build can send them: the settings show the switch. */
  val status: StateFlow<CrashReportsStatus?> = statusFlow

  /** The user's switch (Settings → Privacy and security). */
  val isAllowedByUser: Boolean get() = !prefs.getBoolean(KEY_OPTED_OUT, false)

  /** Whether the first-start notice is due: reports on, the instance wants it, not shown yet. */
  val isNoticeDue: Boolean
    get() = statusFlow.value?.showsNotice == true && !prefs.getBoolean(KEY_NOTICE_SEEN, false)

  /** At process start, before anything else can crash: the last known setting and choice. */
  fun install() = apply()

  @Volatile private var lastRefreshAtMs = 0L

  /**
   * Asks the signed-in [server] for its current setting (`GET /api/v1/server-info`): after sign-in
   * and when the app comes to the foreground, at most once a minute. Offline, or an answer that
   * isn't the server's, keeps the last known setting.
   */
  suspend fun refresh(server: StoredServer) {
    val now = SystemClock.elapsedRealtime()
    if (lastRefreshAtMs != 0L && now - lastRefreshAtMs < REFRESH_INTERVAL_MS) return
    lastRefreshAtMs = now
    val answer = try {
      ServerInfoClient.fetch(server.address.serverInfoUrl)
    } catch (failure: IOException) {
      return
    }
    if (answer.status != HTTP_OK) return
    val maybeConfig = withContext(Dispatchers.Default) { crashReportsConfig(answer.body.decodeToString()) }
    updateConfig(
      maybeConfig?.let {
        CrashConfig(it.dsn, it.sampleRate, it.environment, it.allowsOptOut, it.showsNotice, it.traceRate, it.slowJobMs.toLong())
      },
    )
  }

  /** The signed-in server's current setting (`None` while it has reports off). */
  fun updateConfig(maybeConfig: CrashConfig?) {
    val editor = prefs.edit()
    if (maybeConfig == null) editor.remove(KEY_CONFIG) else editor.putString(KEY_CONFIG, Json.encodeToString(CrashConfig.serializer(), maybeConfig))
    editor.apply()
    statusFlow.value = status(maybeConfig)
    apply()
  }

  /** The user turned the switch; turned on again, reports get a new installation ID. */
  fun setAllowedByUser(isAllowed: Boolean) {
    val editor = prefs.edit().putBoolean(KEY_OPTED_OUT, !isAllowed)
    if (isAllowed && !isAllowedByUser) editor.putString(KEY_INSTALLATION, newInstallationId())
    editor.apply()
    apply()
  }

  fun markNoticeSeen() {
    prefs.edit().putBoolean(KEY_NOTICE_SEEN, true).apply()
  }

  /** The user left this server (signed out and changed the server): its setting no longer counts. */
  fun forgetServer() {
    lastRefreshAtMs = 0L
    updateConfig(null)
  }

  private fun apply() {
    val maybeConfig = maybeConfig()
    if (maybeConfig != null && isActive(maybeConfig, isAllowedByUser)) {
      sdk.start(context, CrashSdkSettings(maybeConfig, "teamcraft@${BuildConfig.VERSION_NAME}", installationId()))
    } else {
      sdk.stop()
    }
  }

  private fun maybeConfig(): CrashConfig? =
    prefs.getString(KEY_CONFIG, null)?.let { json ->
      runCatching { Json.decodeFromString(CrashConfig.serializer(), json) }.getOrNull()
    }

  private fun status(maybeConfig: CrashConfig?): CrashReportsStatus? =
    maybeConfig?.takeIf { sdk.isAvailable }?.let { CrashReportsStatus(it.allowsOptOut, it.showsNotice) }

  private fun installationId(): String =
    prefs.getString(KEY_INSTALLATION, null) ?: newInstallationId().also { prefs.edit().putString(KEY_INSTALLATION, it).apply() }

  companion object {
    private const val FILE = "crash_reports"
    private const val KEY_CONFIG = "config"
    private const val KEY_OPTED_OUT = "opted_out"
    private const val KEY_INSTALLATION = "installation_id"
    private const val KEY_NOTICE_SEEN = "notice_seen"
    private const val HTTP_OK = 200

    /** Foreground starts closer together than this ask the server only once. */
    private const val REFRESH_INTERVAL_MS = 60_000L

    /** On while the instance has reports on, unless the user opted out where that is allowed. */
    fun isActive(config: CrashConfig, isAllowedByUser: Boolean): Boolean = isAllowedByUser || !config.allowsOptOut

    /** 128 random bits as 32 hex digits; never derived from the account, device or instance. */
    fun newInstallationId(): String {
      val bytes = ByteArray(16).also(SecureRandom()::nextBytes)
      return bytes.joinToString("") { "%02x".format(it) }
    }
  }
}

/** The instance's crash reports setting, as the core checked it (`crashReportsConfig`). */
@Serializable
data class CrashConfig(
  val dsn: String,
  /** Share of handled errors sent, 0 to 1; crashes are always sent. */
  val sampleRate: Double,
  val environment: String,
  val allowsOptOut: Boolean,
  val showsNotice: Boolean,
  /** Share of app starts, screen loads and core measures traced, 0 to 1; absent when stored by an older version. */
  val traceRate: Double = 0.0,
  /** A core job that waits or runs this long is reported as a sampled warning; 0: never. */
  val slowJobMs: Long = 1_000,
)

/** What the privacy settings and the first-start notice need to know. */
data class CrashReportsStatus(val allowsOptOut: Boolean, val showsNotice: Boolean)
