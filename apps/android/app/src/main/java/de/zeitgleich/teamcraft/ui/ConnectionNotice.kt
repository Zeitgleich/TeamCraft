package de.zeitgleich.teamcraft.ui

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import uniffi.teamcraft_ffi.ConnectionState
import uniffi.teamcraft_ffi.NetworkInterception

/**
 * The connection banner stays away while the app reconnects on its own: it shows only when the
 * connection is still down this long after it went down or after the app came back to the
 * foreground (issue #700; the same rule as desktop and web, #685). The cached conversations are on
 * screen meanwhile.
 */
const val CONNECTION_GRACE_MS = 10_000L

/** What the connection banner says. */
sealed interface ConnectionNotice {
  /** The device has no validated network. */
  data object DeviceOffline : ConnectionNotice

  data object Connecting : ConnectionNotice

  /**
   * Online, but the server isn't reachable; the next attempt is in [retryInSeconds]. Names what
   * answered instead, if the core found out (a captive portal, a TLS-intercepting proxy).
   */
  data class Unreachable(val retryInSeconds: Long, val maybeInterception: NetworkInterception?) : ConnectionNotice

  /** The server refused the token; shown at once, waiting doesn't fix it. */
  data object SignInRequired : ConnectionNotice
}

/**
 * Since when the connection is down, for the grace period. Unlike web, the device being offline
 * gets the grace period too: Android drops a network's "validated" flag for a moment on every
 * hand-over (Wi-Fi to mobile, leaving Doze), which must not flash a banner.
 */
data class Unsettled(val maybeSinceMs: Long? = null) {
  /** Follows the connection: the grace period starts when it goes down and ends when it is up. */
  fun observe(isDown: Boolean, nowMs: Long): Unsettled = Unsettled(if (isDown) maybeSinceMs ?: nowMs else null)

  /** The app is back in the foreground: a connection that is down gets the grace period from now. */
  fun appReturned(nowMs: Long): Unsettled = Unsettled(maybeSinceMs?.let { nowMs })

  /** Down for longer than the grace period. A clock that went back counts as just now. */
  fun isPastGrace(nowMs: Long): Boolean = maybeSinceMs?.let { nowMs - it >= CONNECTION_GRACE_MS } == true
}

/** Whether the app counts as not connected: no network, or the core is (re)connecting. */
fun isConnectionDown(isOnline: Boolean, maybeConnection: ConnectionState?): Boolean =
  !isOnline || maybeConnection is ConnectionState.Connecting || maybeConnection is ConnectionState.Offline

/**
 * The banner for this moment, or none. The device's own connectivity wins: without a network the
 * server can't be the problem. No core open (`null`): nothing to say about the server.
 */
fun connectionNotice(isOnline: Boolean, maybeConnection: ConnectionState?, unsettled: Unsettled, nowMs: Long): ConnectionNotice? {
  if (maybeConnection is ConnectionState.SignInRequired) return ConnectionNotice.SignInRequired
  if (!unsettled.isPastGrace(nowMs)) return null
  if (!isOnline) return ConnectionNotice.DeviceOffline
  return when (maybeConnection) {
    is ConnectionState.Connecting -> ConnectionNotice.Connecting
    is ConnectionState.Offline -> ConnectionNotice.Unreachable(
      retryInSeconds = ((maybeConnection.retryAtMs - nowMs).coerceAtLeast(0) + 999) / 1000,
      maybeInterception = maybeConnection.maybeInterception,
    )
    else -> null
  }
}

/**
 * The process-wide grace period, so it survives moving between screens and starts again when the
 * app returns to the foreground ([appReturned], from the process lifecycle). The banner reads it.
 */
class ConnectionGrace(
  scope: CoroutineScope,
  connection: StateFlow<ConnectionState?>,
  isOnline: StateFlow<Boolean>,
  private val clock: () -> Long = System::currentTimeMillis,
) {
  private val current = MutableStateFlow(Unsettled())

  val unsettled: StateFlow<Unsettled> = current

  init {
    scope.launch {
      combine(connection, isOnline) { maybeConnection, online -> isConnectionDown(online, maybeConnection) }
        .collect { isDown -> current.update { it.observe(isDown, clock()) } }
    }
  }

  fun appReturned() = current.update { it.appReturned(clock()) }
}
