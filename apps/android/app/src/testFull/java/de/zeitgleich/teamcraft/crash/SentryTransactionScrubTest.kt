package de.zeitgleich.teamcraft.crash

import io.sentry.JsonSerializer
import io.sentry.SentryOptions
import io.sentry.SpanId
import io.sentry.SpanStatus
import io.sentry.protocol.Request
import io.sentry.protocol.SentryId
import io.sentry.protocol.SentrySpan
import io.sentry.protocol.SentryTransaction
import io.sentry.protocol.TransactionInfo
import io.sentry.protocol.User
import java.io.StringWriter
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

private const val INSTALLATION_ID = "0123456789abcdef0123456789abcdef"

class SentryTransactionScrubTest {
  @Test
  fun aTransactionKeepsItsTimesAndLosesEverythingPersonal() {
    val scrubbed = SentryTransactionScrub.prepare(leakyTransaction(), INSTALLATION_ID)
    val json = serialize(scrubbed)
    for (secret in SECRETS) assertFalse("$secret leaked into $json", json.contains(secret))
    assertEquals(INSTALLATION_ID, scrubbed.user?.id)
    assertNull(scrubbed.user?.email)
    assertNull(scrubbed.request)
    assertNull(scrubbed.serverName)
    assertEquals(1, scrubbed.spans.size)
    val span = scrubbed.spans.single()
    assertEquals("https://team.example.com/api/v1/conversations/7/messages?[redacted]", span.description)
    assertTrue(span.data.isNullOrEmpty())
    assertEquals(1.0, span.startTimestamp, 0.0)
    assertEquals(2.0, span.timestamp!!, 0.0)
    assertTrue(json.contains("ui.load"))
  }

  private fun leakyTransaction(): SentryTransaction {
    val span = SentrySpan(
      1.0,
      2.0,
      SentryId(),
      SpanId(),
      SpanId(),
      "http.client",
      "https://team.example.com/api/v1/conversations/7/messages?access_token=SECRET-TOKEN-123",
      SpanStatus.OK,
      "auto.http",
      mapOf("http.query" to "q=ALICE-EXAMPLE"),
      emptyMap(),
      mutableMapOf<String, Any>("url" to "https://team.example.com/x?access_token=SECRET-TOKEN-123", "file.path" to "/data/user/0/Quarterly-Report.pdf"),
    )
    return SentryTransaction("MainActivity", 0.0, 3.0, mutableListOf(span), emptyMap(), TransactionInfo("component")).apply {
      contexts.setTrace(io.sentry.SpanContext("ui.load"))
      request = Request().apply { url = "https://team.example.com/?token=SECRET-TOKEN-123" }
      serverName = "alice-phone"
      user = User().apply {
        email = "alice@example.com"
        username = "ALICE-EXAMPLE"
      }
      setExtra("draft", "MESSAGE-TEXT")
      setTag("screen", "opened by alice@example.com")
    }
  }

  private fun serialize(transaction: SentryTransaction): String {
    val writer = StringWriter()
    JsonSerializer(SentryOptions()).serialize(transaction, writer)
    return writer.toString()
  }

  private companion object {
    val SECRETS = listOf("SECRET-TOKEN-123", "ALICE-EXAMPLE", "alice@example.com", "alice-phone", "MESSAGE-TEXT", "Quarterly-Report")
  }
}
