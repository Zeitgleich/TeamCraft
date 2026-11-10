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

  @Test
  fun aScreenLoadWhoseFirstFrameWaitedWhileHiddenIsUnseen() {
    assertTrue(SentryTransactionScrub.isUnseenScreenLoad(screenLoad(initialDisplaySeconds = 746.0)))
    assertTrue(SentryTransactionScrub.isUnseenScreenLoad(screenLoad(initialDisplaySeconds = 10.5)))
  }

  @Test
  fun aScreenLoadDrawnInTimeIsSeen() {
    assertFalse(SentryTransactionScrub.isUnseenScreenLoad(screenLoad(initialDisplaySeconds = 0.4)))
    assertFalse(SentryTransactionScrub.isUnseenScreenLoad(screenLoad(initialDisplaySeconds = 10.0)))
  }

  @Test
  fun onlyScreenLoadsAreJudged() {
    assertFalse(SentryTransactionScrub.isUnseenScreenLoad(screenLoad(initialDisplaySeconds = 60.0, op = "app.start")))
    // Not finished: nothing to judge by.
    assertFalse(SentryTransactionScrub.isUnseenScreenLoad(screenLoad(initialDisplaySeconds = null)))
    assertFalse(SentryTransactionScrub.isUnseenScreenLoad(transaction(op = "ui.load", spans = emptyList())))
  }

  private fun screenLoad(initialDisplaySeconds: Double?, op: String = "ui.load"): SentryTransaction {
    val created = span("activity.load", 100.0, 100.2)
    val initialDisplay = span("ui.load.initial_display", 100.0, initialDisplaySeconds?.let { 100.0 + it })
    return transaction(op, listOf(created, initialDisplay))
  }

  private fun span(op: String, start: Double, end: Double?) =
    SentrySpan(start, end, SentryId(), SpanId(), SpanId(), op, "MainActivity", SpanStatus.OK, "auto.ui.activity", emptyMap(), emptyMap(), null)

  private fun transaction(op: String, spans: List<SentrySpan>) =
    SentryTransaction("MainActivity", 100.0, 900.0, spans.toMutableList(), emptyMap(), TransactionInfo("component")).apply {
      contexts.setTrace(io.sentry.SpanContext(op))
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
