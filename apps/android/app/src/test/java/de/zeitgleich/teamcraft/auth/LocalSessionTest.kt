package de.zeitgleich.teamcraft.auth

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

private const val NOW = 1_800_000_000_000L
private const val HOUR = 3_600_000L
private const val TOKEN = "token-for-tests"
private const val USER_ID = "123456789"

class LocalSessionTest {
  class Serialization {
    @Test
    fun aSessionRoundTrips() {
      val session = LocalSession(TOKEN, NOW + HOUR, USER_ID, clockOffsetMillis = -5_000)
      assertEquals(session, LocalSession.decode(session.encode()))
    }

    @Test
    fun unreadableOrIncompleteDataIsNoSession() {
      assertNull(LocalSession.decode(ByteArray(0)))
      assertNull(LocalSession.decode("not json".encodeToByteArray()))
      assertNull(LocalSession.decode("""{"access_token":"","expires_at":1,"user_id":"1"}""".encodeToByteArray()))
      assertNull(LocalSession.decode("""{"access_token":"a","expires_at":1}""".encodeToByteArray()))
    }

    @Test
    fun theTokenNeverShowsInToString() {
      assertFalse(LocalSession(TOKEN, NOW, USER_ID).toString().contains(TOKEN))
      assertFalse(LocalLoginOutcome.SignedIn(LocalSession(TOKEN, NOW, USER_ID)).toString().contains(TOKEN))
    }
  }

  class Expiry {
    @Test
    fun aSessionIsValidUntilShortlyBeforeItsEnd() {
      val session = LocalSession(TOKEN, NOW + HOUR, USER_ID)
      assertTrue(session.isValid(NOW))
      assertTrue(session.isValid(NOW + HOUR - 31_000))
      assertFalse(session.isValid(NOW + HOUR - 29_000))
      assertFalse(session.isValid(NOW + 2 * HOUR))
    }

    @Test
    fun theServerClockDecides() {
      // The device clock is two hours behind the server: by the server's clock the hour is over.
      val behind = LocalSession(TOKEN, NOW + HOUR, USER_ID, clockOffsetMillis = 2 * HOUR)
      assertFalse(behind.isValid(NOW))
      // The device clock is two hours ahead: the session is still good.
      val ahead = LocalSession(TOKEN, NOW + HOUR, USER_ID, clockOffsetMillis = -2 * HOUR)
      assertTrue(ahead.isValid(NOW + 2 * HOUR))
    }
  }

  class Answers {
    @Test
    fun aGrantBecomesASessionWithTheClockOffset() {
      val body = """{"access_token":"$TOKEN","expires_at":${NOW + HOUR},"user_id":"$USER_ID","extra":1}"""
      val outcome = localLoginOutcome(200, body.encodeToByteArray(), maybeServerDateMillis = NOW + 1_000, deviceNowMillis = NOW)
      assertEquals(LocalLoginOutcome.SignedIn(LocalSession(TOKEN, NOW + HOUR, USER_ID, 1_000)), outcome)
    }

    @Test
    fun withoutADateTheDeviceClockIsUsed() {
      val body = """{"access_token":"$TOKEN","expires_at":${NOW + HOUR},"user_id":"$USER_ID"}"""
      val outcome = localLoginOutcome(200, body.encodeToByteArray(), maybeServerDateMillis = null, deviceNowMillis = NOW)
      assertEquals(0L, (outcome as LocalLoginOutcome.SignedIn).session.clockOffsetMillis)
    }

    @Test
    fun anHtmlPageOrARedirectIsAnInterception() {
      assertEquals(LocalLoginOutcome.Intercepted, outcome(200, "<html>Hotel Wi-Fi</html>"))
      assertEquals(LocalLoginOutcome.Intercepted, outcome(302, ""))
    }

    @Test
    fun wrongCredentialsAndAMissingCodeReadTheSame() {
      assertEquals(LocalLoginOutcome.Rejected, outcome(401, error("unauthorized")))
      assertEquals(LocalLoginOutcome.Rejected, outcome(401, error("mfa_required")))
      assertEquals(LocalLoginOutcome.Rejected, outcome(400, error("invalid_request")))
      assertEquals(LocalLoginOutcome.Rejected, outcome(401, ""))
    }

    @Test
    fun errorCodesAndStatusesMapToTheirOutcome() {
      assertEquals(LocalLoginOutcome.Disabled, outcome(403, error("local_accounts_disabled")))
      assertEquals(LocalLoginOutcome.Disabled, outcome(503, error("something_new")))
      assertEquals(LocalLoginOutcome.RateLimited, outcome(429, error("rate_limited")))
      assertEquals(LocalLoginOutcome.RateLimited, outcome(429, ""))
      assertEquals(LocalLoginOutcome.Unavailable, outcome(502, "<html>Bad gateway</html>"))
      assertEquals(LocalLoginOutcome.Failed, outcome(404, ""))
    }
  }
}

private fun outcome(status: Int, body: String): LocalLoginOutcome =
  localLoginOutcome(status, body.encodeToByteArray(), maybeServerDateMillis = null, deviceNowMillis = NOW)

private fun error(code: String): String = """{"error_code":"$code","message":"for tests"}"""
