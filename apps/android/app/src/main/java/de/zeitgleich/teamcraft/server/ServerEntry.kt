package de.zeitgleich.teamcraft.server

import java.io.IOException
import java.net.UnknownServiceException
import javax.net.ssl.SSLException
import androidx.annotation.StringRes
import de.zeitgleich.teamcraft.R
import uniffi.teamcraft_ffi.ServerEntryException

/*
 * The decisions of the server entry (docs/operations.md "Server address in the apps") that don't
 * need Android or the Rust library, so unit tests cover them: where the app starts, which server a
 * lock allows, how files are keyed by server, and which text explains a failure.
 */

/** Hosts plain HTTP may always reach, also in release builds (a server on the device itself). */
private val LOOPBACK_HOSTS = setOf("localhost", "127.0.0.1", "::1", "[::1]")

/** A server named before the user types anything: by the managed configuration or the build. */
data class ServerPreset(val address: String, val source: PresetSource, val isLocked: Boolean)

/** Who named a [ServerPreset]. */
enum class PresetSource {
  /** The managed configuration (MDM / Android Enterprise); may lock the server. */
  MANAGED,

  /** `teamcraft.defaultServer` of a self-built app; never locked. */
  BUILD,
}

/**
 * The only server a locked managed configuration allows. [maybeOrigin] is the normalised origin of
 * [address]; `null` when the configured address is invalid, in which case no server is allowed.
 */
data class ServerLock(val address: String, val maybeOrigin: String?)

/** Where the app goes at start, before it asks the identity provider anything. */
sealed interface ServerStart {
  /** Go on with the stored account's server. */
  data object UseStored : ServerStart

  /** The stored server is not the locked one: sign out, forget it and check the locked server. */
  data class ReplaceStored(val address: String) : ServerStart

  /** No account yet, but a preset server: check it and ask the user to confirm. */
  data class CheckPreset(val preset: ServerPreset) : ServerStart

  /** No account and no preset: ask for the server address. */
  data object AskForServer : ServerStart
}

/**
 * The preset server: the managed configuration's wins over the build's default. A lock without a
 * managed address has nothing to lock to and is ignored.
 */
fun choosePreset(maybeManagedUrl: String?, isManagedLock: Boolean, buildDefault: String): ServerPreset? {
  maybeManagedUrl?.trim()?.takeIf { it.isNotEmpty() }?.let {
    return ServerPreset(it, PresetSource.MANAGED, isLocked = isManagedLock)
  }
  return buildDefault.trim().takeIf { it.isNotEmpty() }?.let {
    ServerPreset(it, PresetSource.BUILD, isLocked = false)
  }
}

/** Decides the start; [maybeLock] is set when the managed configuration locks the server. */
fun decideServerStart(maybeStoredOrigin: String?, maybePreset: ServerPreset?, maybeLock: ServerLock?): ServerStart =
  when {
    maybeStoredOrigin != null && isServerAllowed(maybeStoredOrigin, maybeLock) -> ServerStart.UseStored
    maybeStoredOrigin != null && maybeLock != null -> ServerStart.ReplaceStored(maybeLock.address)
    maybePreset != null -> ServerStart.CheckPreset(maybePreset)
    else -> ServerStart.AskForServer
  }

/** Whether the app may use the server at [origin]: any without a lock, only the locked one with. */
fun isServerAllowed(origin: String, maybeLock: ServerLock?): Boolean = maybeLock == null || maybeLock.maybeOrigin == origin

/**
 * Whether plain HTTP may go to [maybeHost]: always to the device itself, anywhere else only in
 * development builds ([allowInsecureHttp]).
 */
fun isPlainHttpAllowed(maybeHost: String?, allowInsecureHttp: Boolean): Boolean =
  allowInsecureHttp || maybeHost?.lowercase() in LOOPBACK_HOSTS

/** Why a typed address or the server's answer can't be used, in words the user can act on. */
@StringRes
fun serverEntryErrorText(failure: ServerEntryException): Int = when (failure) {
  is ServerEntryException.Empty -> R.string.server_error_empty
  is ServerEntryException.InvalidAddress -> R.string.server_error_invalid
  is ServerEntryException.HasCredentials -> R.string.server_error_credentials
  is ServerEntryException.HttpsOnly -> R.string.server_error_https_only
  is ServerEntryException.NotTeamCraft -> R.string.server_error_not_teamcraft
  is ServerEntryException.CaptivePortal -> R.string.server_error_captive_portal
  is ServerEntryException.Unavailable -> R.string.server_error_unavailable
  is ServerEntryException.AppTooOld -> R.string.server_error_app_too_old
  is ServerEntryException.ServerTooOld -> R.string.server_error_server_too_old
  is ServerEntryException.InvalidSignIn -> R.string.server_error_invalid_sign_in
  is ServerEntryException.RedirectNotAllowed -> R.string.server_error_redirect_not_allowed
}

/** Why fetching the server's answer failed on the way there. */
@StringRes
fun transportErrorText(failure: IOException, isOnline: Boolean): Int = when {
  failure is SSLException -> R.string.server_error_tls
  failure is ResponseTooLargeException -> R.string.server_error_not_teamcraft
  // The network security config refused plain HTTP: the address is the problem, not the network.
  failure is UnknownServiceException -> R.string.server_error_https_only
  !isOnline -> R.string.server_error_offline
  else -> R.string.server_error_unreachable
}

/** Whether a failure is one that may go away by itself once the device is (back) online. */
fun isRetryableWhenOnline(@StringRes error: Int): Boolean =
  error == R.string.server_error_offline || error == R.string.server_error_unreachable

/**
 * Where an account's files live, keyed by its server's storage key so two servers never share
 * a session, a store or a database key. Paths are relative to the app's files directory.
 */
object StoragePaths {
  /** The sealed OIDC session of the account on a server. */
  fun sessionFile(storageKey: String): String = "auth/${segment(storageKey)}/session.bin"

  /** The sealed session of a local account (e-mail and password) on a server. */
  fun localSessionFile(storageKey: String): String = "auth/${segment(storageKey)}/local-session.bin"

  /** The cached icon of a server's identity provider ("Sign in with …"). */
  fun ssoIconFile(storageKey: String): String = "${brandDir(storageKey)}/sso_icon"

  /** The core's data directory of a server; the core adds a directory per account. */
  fun storeDir(storageKey: String): String = "store/${segment(storageKey)}"

  /** The sealed database key of the account [subject] on a server. */
  fun databaseKeyFile(storageKey: String, subject: String): String = "keys/${segment(storageKey)}/${storeName(subject)}.key"

  /** The cached brand logos of a server. */
  fun brandDir(storageKey: String): String = "brand/${segment(storageKey)}"

  /** A file-name-safe name for an OIDC subject (UUIDs pass unchanged). */
  fun storeName(subject: String): String =
    subject.map { if (it.isLetterOrDigit() || it == '-' || it == '_') it else '_' }.joinToString("")

  /**
   * The storage key as one path segment. The core already returns file-name-safe keys; this
   * guards against a separator or a dot-only name all the same.
   */
  internal fun segment(storageKey: String): String {
    val safe = storageKey.map { if (it.isLetterOrDigit() || it in ".-_") it else '_' }.joinToString("")
    return if (safe.isEmpty() || safe.all { it == '.' }) "_" else safe
  }
}
