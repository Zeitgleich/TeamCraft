package de.zeitgleich.teamcraft.server

import java.io.IOException
import java.net.ConnectException
import java.net.SocketTimeoutException
import java.net.UnknownHostException
import java.net.UnknownServiceException
import javax.net.ssl.SSLHandshakeException
import de.zeitgleich.teamcraft.R
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.teamcraft_ffi.ServerEntryException

private const val ORIGIN = "https://team.example.com"
private const val OTHER_ORIGIN = "https://other.example.com"

class ServerEntryTest {
  class Presets {
    @Test
    fun theManagedServerWinsOverTheBuildDefault() {
      val preset = choosePreset(" https://team.example.com ", isManagedLock = true, buildDefault = OTHER_ORIGIN)
      assertEquals(ServerPreset(ORIGIN, PresetSource.MANAGED, isLocked = true), preset)
    }

    @Test
    fun theBuildDefaultIsNeverLocked() {
      assertEquals(
        ServerPreset(OTHER_ORIGIN, PresetSource.BUILD, isLocked = false),
        choosePreset(maybeManagedUrl = "  ", isManagedLock = true, buildDefault = OTHER_ORIGIN),
      )
    }

    @Test
    fun aLockWithoutAnAddressIsIgnored() {
      assertNull(choosePreset(maybeManagedUrl = null, isManagedLock = true, buildDefault = ""))
      assertNull(choosePreset(maybeManagedUrl = "", isManagedLock = false, buildDefault = " "))
    }
  }

  class Start {
    @Test
    fun aStoredServerIsUsedWithoutALock() {
      val preset = ServerPreset(OTHER_ORIGIN, PresetSource.MANAGED, isLocked = false)
      assertEquals(ServerStart.UseStored, decideServerStart(ORIGIN, preset, maybeLock = null))
      assertEquals(ServerStart.UseStored, decideServerStart(ORIGIN, maybePreset = null, maybeLock = null))
    }

    @Test
    fun aStoredServerIsUsedWhenItIsTheLockedOne() {
      val lock = ServerLock("team.example.com", ORIGIN)
      assertEquals(ServerStart.UseStored, decideServerStart(ORIGIN, lockedPreset(), lock))
    }

    @Test
    fun aStoredServerOtherThanTheLockedOneIsReplaced() {
      val lock = ServerLock("team.example.com", ORIGIN)
      assertEquals(ServerStart.ReplaceStored("team.example.com"), decideServerStart(OTHER_ORIGIN, lockedPreset(), lock))
    }

    @Test
    fun anInvalidLockedAddressAllowsNoStoredServer() {
      val lock = ServerLock("not an address", maybeOrigin = null)
      assertEquals(ServerStart.ReplaceStored("not an address"), decideServerStart(ORIGIN, lockedPreset(), lock))
    }

    @Test
    fun withoutAnAccountAPresetIsCheckedAndOtherwiseTheUserIsAsked() {
      val preset = ServerPreset(ORIGIN, PresetSource.BUILD, isLocked = false)
      assertEquals(ServerStart.CheckPreset(preset), decideServerStart(null, preset, maybeLock = null))
      assertEquals(ServerStart.AskForServer, decideServerStart(null, maybePreset = null, maybeLock = null))
    }
  }

  class Lock {
    @Test
    fun onlyTheLockedServerIsAllowed() {
      assertTrue(isServerAllowed(OTHER_ORIGIN, maybeLock = null))
      assertTrue(isServerAllowed(ORIGIN, ServerLock("team.example.com", ORIGIN)))
      assertFalse(isServerAllowed(OTHER_ORIGIN, ServerLock("team.example.com", ORIGIN)))
      assertFalse(isServerAllowed(ORIGIN, ServerLock("not an address", maybeOrigin = null)))
    }

    @Test
    fun plainHttpGoesOnlyToTheDeviceItselfOutsideDevelopmentBuilds() {
      assertTrue(isPlainHttpAllowed("127.0.0.1", allowInsecureHttp = false))
      assertTrue(isPlainHttpAllowed("LOCALHOST", allowInsecureHttp = false))
      assertTrue(isPlainHttpAllowed("[::1]", allowInsecureHttp = false))
      assertFalse(isPlainHttpAllowed("10.0.2.2", allowInsecureHttp = false))
      assertFalse(isPlainHttpAllowed("team.example.com", allowInsecureHttp = false))
      assertFalse(isPlainHttpAllowed(null, allowInsecureHttp = false))
      assertTrue(isPlainHttpAllowed("10.0.2.2", allowInsecureHttp = true))
    }
  }

  class Errors {
    @Test
    fun everyServerEntryErrorHasItsOwnText() {
      val texts = mapOf(
        ServerEntryException.Empty("") to R.string.server_error_empty,
        ServerEntryException.InvalidAddress("") to R.string.server_error_invalid,
        ServerEntryException.HasCredentials("") to R.string.server_error_credentials,
        ServerEntryException.HttpsOnly("") to R.string.server_error_https_only,
        ServerEntryException.NotTeamCraft("") to R.string.server_error_not_teamcraft,
        ServerEntryException.CaptivePortal("") to R.string.server_error_captive_portal,
        ServerEntryException.Unavailable("HTTP 503") to R.string.server_error_unavailable,
        ServerEntryException.AppTooOld("2.0.0") to R.string.server_error_app_too_old,
        ServerEntryException.ServerTooOld("0.9.0") to R.string.server_error_server_too_old,
        ServerEntryException.InvalidSignIn("no issuer") to R.string.server_error_invalid_sign_in,
        ServerEntryException.RedirectNotAllowed("") to R.string.server_error_redirect_not_allowed,
      )
      for ((failure, text) in texts) assertEquals(failure.javaClass.simpleName, text, serverEntryErrorText(failure))
      assertEquals(texts.size, texts.values.toSet().size)
    }

    @Test
    fun tlsProblemsAreToldApartFromUnreachableServers() {
      assertEquals(R.string.server_error_tls, transportErrorText(SSLHandshakeException("bad certificate"), isOnline = true))
      assertEquals(R.string.server_error_tls, transportErrorText(SSLHandshakeException("bad certificate"), isOnline = false))
      assertEquals(R.string.server_error_unreachable, transportErrorText(UnknownHostException(), isOnline = true))
      assertEquals(R.string.server_error_unreachable, transportErrorText(ConnectException(), isOnline = true))
      assertEquals(R.string.server_error_unreachable, transportErrorText(SocketTimeoutException(), isOnline = true))
      assertEquals(R.string.server_error_unreachable, transportErrorText(IOException(), isOnline = true))
    }

    @Test
    fun offlineIsSaidAsSuch() {
      assertEquals(R.string.server_error_offline, transportErrorText(UnknownHostException(), isOnline = false))
      assertEquals(R.string.server_error_offline, transportErrorText(ConnectException(), isOnline = false))
    }

    @Test
    fun oversizedAnswersAndRefusedPlainHttpPointAtTheAddress() {
      assertEquals(R.string.server_error_not_teamcraft, transportErrorText(ResponseTooLargeException(), isOnline = true))
      assertEquals(R.string.server_error_https_only, transportErrorText(UnknownServiceException("cleartext"), isOnline = true))
    }

    @Test
    fun onlyConnectivityFailuresAreRetriedWhenBackOnline() {
      assertTrue(isRetryableWhenOnline(R.string.server_error_offline))
      assertTrue(isRetryableWhenOnline(R.string.server_error_unreachable))
      assertFalse(isRetryableWhenOnline(R.string.server_error_tls))
      assertFalse(isRetryableWhenOnline(R.string.server_error_not_teamcraft))
    }
  }

  class Paths {
    @Test
    fun everythingOfAnAccountIsKeyedByItsServer() {
      assertEquals("auth/team.example.com/session.bin", StoragePaths.sessionFile("team.example.com"))
      assertEquals("store/team.example.com_8443", StoragePaths.storeDir("team.example.com_8443"))
      assertEquals(
        "keys/http_localhost_8081/0b6f-4c1a_x.key",
        StoragePaths.databaseKeyFile("http_localhost_8081", "0b6f-4c1a|x"),
      )
      assertEquals("brand/team.example.com", StoragePaths.brandDir("team.example.com"))
    }

    @Test
    fun aStorageKeyNeverLeavesItsDirectory() {
      assertEquals("_", StoragePaths.segment(""))
      assertEquals("_", StoragePaths.segment(".."))
      assertEquals(".._etc", StoragePaths.segment("../etc"))
      assertEquals("auth/a_b/session.bin", StoragePaths.sessionFile("a/b"))
    }
  }
}

private fun lockedPreset() = ServerPreset("team.example.com", PresetSource.MANAGED, isLocked = true)
