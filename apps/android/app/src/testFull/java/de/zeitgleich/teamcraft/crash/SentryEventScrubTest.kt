package de.zeitgleich.teamcraft.crash

import io.sentry.JsonSerializer
import io.sentry.SentryEvent
import io.sentry.SentryLevel
import io.sentry.SentryOptions
import io.sentry.protocol.Mechanism
import io.sentry.protocol.Message
import io.sentry.protocol.Request
import io.sentry.protocol.SentryException
import io.sentry.protocol.User
import java.io.StringWriter
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

private const val INSTALLATION = "0123456789abcdef0123456789abcdef"

class SentryEventScrubTest {
  @Test
  fun aReportFromASessionWithSecretsContainsNoneOfThem() {
    val event = SentryEventScrub.prepare(leakyEvent(isCrash = true), INSTALLATION, sampleRate = 0.0, roll = 0.5) { LEAKY_TEXTS }
    val json = serialize(assertNotNullEvent(event))
    for (secret in SECRETS) assertFalse("$secret leaked into $json", json.contains(secret))
    // What helps fixing it is still there.
    assertTrue(json.contains("IllegalStateException"))
    assertTrue(json.contains("Failed to send"))
    assertTrue(json.contains(INSTALLATION))
    assertTrue(json.contains("https://cdn.example.com/upload?[redacted]"))
  }

  @Test
  fun onlyTheInstallationIdIsLeftOfTheUser() {
    val event = assertNotNullEvent(SentryEventScrub.prepare(leakyEvent(isCrash = true), INSTALLATION, 1.0, 0.0) { emptyList() })
    assertEquals(INSTALLATION, event.user?.id)
    assertNull(event.user?.email)
    assertNull(event.user?.username)
    assertNull(event.user?.ipAddress)
    assertNull(event.request)
    assertNull(event.serverName)
    assertNull(event.getExtra("draft"))
  }

  @Test
  fun breadcrumbsAreTheNewestLogLinesOnly() {
    val lines = (1..80).map { "line $it" }
    val event = assertNotNullEvent(SentryEventScrub.prepare(leakyEvent(isCrash = true), INSTALLATION, 1.0, 0.0) { lines })
    val breadcrumbs = event.breadcrumbs.orEmpty()
    assertEquals(CrashScrubber.MAX_BREADCRUMBS, breadcrumbs.size)
    assertEquals("line 31", breadcrumbs.first().message)
    assertEquals("line 80", breadcrumbs.last().message)
  }

  @Test
  fun crashesAreAlwaysSentHandledErrorsSampled() {
    assertNotNull(SentryEventScrub.prepare(leakyEvent(isCrash = true), INSTALLATION, 0.0, 0.99) { emptyList() })
    assertNull(SentryEventScrub.prepare(leakyEvent(isCrash = false), INSTALLATION, 0.1, 0.5) { emptyList() })
    assertNotNull(SentryEventScrub.prepare(leakyEvent(isCrash = false), INSTALLATION, 0.1, 0.05) { emptyList() })
  }

  private fun leakyEvent(isCrash: Boolean): SentryEvent =
    SentryEvent().apply {
      level = if (isCrash) SentryLevel.FATAL else SentryLevel.ERROR
      exceptions = listOf(
        SentryException().apply {
          type = "IllegalStateException"
          module = "java.lang"
          value = LEAKY_TEXTS.first()
          mechanism = Mechanism().apply {
            type = "UncaughtExceptionHandler"
            isHandled = !isCrash
          }
        },
      )
      message = Message().apply { formatted = LEAKY_TEXTS[3] }
      user = User().apply {
        id = "alice@example.com"
        email = "alice@example.com"
        username = "Alice Example"
      }
      request = Request().apply { url = "https://team.example.com/s/1#share-link-secret" }
      serverName = "henning-laptop"
      setExtra("draft", "meet me at the harbour at nine")
    }

  private fun serialize(event: SentryEvent): String {
    val writer = StringWriter()
    JsonSerializer(SentryOptions()).serialize(event, writer)
    return writer.toString()
  }

  private fun assertNotNullEvent(event: SentryEvent?): SentryEvent {
    assertNotNull(event)
    return event!!
  }
}
