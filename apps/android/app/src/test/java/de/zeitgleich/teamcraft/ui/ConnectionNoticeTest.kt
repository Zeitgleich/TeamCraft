package de.zeitgleich.teamcraft.ui

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.MutableStateFlow
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import uniffi.teamcraft_ffi.ConnectionState
import uniffi.teamcraft_ffi.NetworkInterception

class ConnectionNoticeTest {
  @Test
  fun aConnectedOrUnknownCoreShowsNothing() {
    assertNull(connectionNotice(true, ConnectionState.Connected, Unsettled(), 0))
    assertNull(connectionNotice(true, null, Unsettled(), 0))
  }

  @Test
  fun aRefusedTokenAsksToSignInAtOnce() {
    assertEquals(ConnectionNotice.SignInRequired, connectionNotice(true, ConnectionState.SignInRequired, Unsettled(), 0))
  }

  @Test
  fun connectingShowsOnlyAfterTheGracePeriod() {
    val unsettled = Unsettled().observe(isDown = true, nowMs = 1_000)
    assertNull(connectionNotice(true, ConnectionState.Connecting, unsettled, 1_000 + CONNECTION_GRACE_MS - 1))
    assertEquals(ConnectionNotice.Connecting, connectionNotice(true, ConnectionState.Connecting, unsettled, 1_000 + CONNECTION_GRACE_MS))
  }

  @Test
  fun theGracePeriodIsTenSecondsLikeDesktopAndWeb() {
    assertEquals(10_000L, CONNECTION_GRACE_MS)
  }

  @Test
  fun aShortNetworkGapShowsNothing() {
    // Android drops "validated" for a moment on a hand-over; no "No connection" flash.
    val unsettled = Unsettled().observe(isDown = true, nowMs = 0)
    assertNull(connectionNotice(false, ConnectionState.Connected, unsettled, 2_000))
    assertEquals(ConnectionNotice.DeviceOffline, connectionNotice(false, ConnectionState.Connected, unsettled, CONNECTION_GRACE_MS))
  }

  @Test
  fun beingOfflineWinsOverTheServerState() {
    val unsettled = Unsettled().observe(isDown = true, nowMs = 0)
    assertEquals(ConnectionNotice.DeviceOffline, connectionNotice(false, offline(retryAtMs = 30_000), unsettled, 20_000))
  }

  @Test
  fun theGracePeriodRunsOnFromLostToConnecting() {
    val unsettled = Unsettled().observe(isDown = true, nowMs = 1_000).observe(isDown = true, nowMs = 5_000)
    assertEquals(ConnectionNotice.Connecting, connectionNotice(true, ConnectionState.Connecting, unsettled, 1_000 + CONNECTION_GRACE_MS))
  }

  @Test
  fun beingConnectedEndsTheGracePeriod() {
    val unsettled = Unsettled()
      .observe(isDown = true, nowMs = 1_000)
      .observe(isDown = false, nowMs = 2_000)
      .observe(isDown = true, nowMs = 20_000)
    assertNull(connectionNotice(true, ConnectionState.Connecting, unsettled, 25_000))
  }

  @Test
  fun anAppBackFromTheBackgroundReconnectsSilently() {
    // Down before the app went to the background; it comes back much later.
    val back = Unsettled().observe(isDown = true, nowMs = 1_000).appReturned(nowMs = 600_000)
    assertNull(connectionNotice(true, ConnectionState.Connecting, back, 600_000))
    assertNull(connectionNotice(true, ConnectionState.Connecting, back, 605_000))
    assertEquals(ConnectionNotice.Connecting, connectionNotice(true, ConnectionState.Connecting, back, 600_000 + CONNECTION_GRACE_MS))
  }

  @Test
  fun aReturnWhileConnectedStartsNoGracePeriod() {
    assertEquals(Unsettled(), Unsettled().appReturned(nowMs = 5_000))
  }

  @Test
  fun aClockThatWentBackShowsNothingYet() {
    val unsettled = Unsettled().observe(isDown = true, nowMs = 10_000)
    assertNull(connectionNotice(true, ConnectionState.Connecting, unsettled, 1_000))
  }

  @Test
  fun unreachableCountsDownToTheNextAttempt() {
    val unsettled = Unsettled().observe(isDown = true, nowMs = 0)
    assertEquals(
      ConnectionNotice.Unreachable(retryInSeconds = 8, maybeInterception = null),
      connectionNotice(true, offline(retryAtMs = 17_500), unsettled, CONNECTION_GRACE_MS),
    )
    // Due but not started yet: connecting, not "next attempt in 0 s".
    assertEquals(ConnectionNotice.Connecting, connectionNotice(true, offline(retryAtMs = 17_500), unsettled, 30_000))
  }

  @Test
  fun aSlowLinkSaysSlowAtOnceAndIsNotDown() {
    assertEquals(ConnectionNotice.Slow, connectionNotice(true, ConnectionState.Slow, Unsettled(), 0))
    assertEquals(false, isConnectionDown(true, ConnectionState.Slow))
    assertEquals(ConnectionNotice.DeviceOffline, connectionNotice(false, ConnectionState.Slow, Unsettled(0), CONNECTION_GRACE_MS))
  }

  @Test
  fun aCaptivePortalIsNamed() {
    val unsettled = Unsettled().observe(isDown = true, nowMs = 0)
    val portal = ConnectionState.Offline(retryAtMs = 12_000, maybeInterception = NetworkInterception.CAPTIVE_PORTAL)
    assertEquals(
      ConnectionNotice.Unreachable(retryInSeconds = 2, maybeInterception = NetworkInterception.CAPTIVE_PORTAL),
      connectionNotice(true, portal, unsettled, CONNECTION_GRACE_MS),
    )
  }

  @Test
  fun whatCountsAsDown() {
    assertEquals(true, isConnectionDown(false, ConnectionState.Connected))
    assertEquals(true, isConnectionDown(true, ConnectionState.Connecting))
    assertEquals(true, isConnectionDown(true, offline(retryAtMs = 0)))
    assertEquals(false, isConnectionDown(true, ConnectionState.Connected))
    assertEquals(false, isConnectionDown(true, ConnectionState.SignInRequired))
    assertEquals(false, isConnectionDown(true, null))
  }

  @Test
  fun theProcessWideGraceFollowsTheFlowsAndTheAppsReturn() {
    val scope = CoroutineScope(Dispatchers.Unconfined)
    val connection = MutableStateFlow<ConnectionState?>(ConnectionState.Connected)
    val online = MutableStateFlow(true)
    var now = 1_000L
    val grace = ConnectionGrace(scope, connection, online) { now }
    assertNull(grace.unsettled.value.maybeSinceMs)
    connection.value = ConnectionState.Connecting
    assertEquals(1_000L, grace.unsettled.value.maybeSinceMs)
    now = 50_000
    grace.appReturned()
    assertEquals(50_000L, grace.unsettled.value.maybeSinceMs)
    connection.value = ConnectionState.Connected
    assertNull(grace.unsettled.value.maybeSinceMs)
    scope.cancel()
  }

  private fun offline(retryAtMs: Long) = ConnectionState.Offline(retryAtMs = retryAtMs, maybeInterception = null)
}
