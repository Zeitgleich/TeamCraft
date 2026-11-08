package de.zeitgleich.teamcraft.crash

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** What a session with secret content could leave behind; none of it may reach a report. */
val SECRETS = listOf(
  "meet me at the harbour at nine",
  "Alice Example",
  "quarterly-figures.xlsx",
  "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.c2lnbmF0dXJl",
  "s3cr3t-pa55word",
  "X-Amz-Signature=",
  "Q2hhbm5lbCBrZXkgbWF0ZXJpYWwgaGVyZQ",
  "alice@example.com",
  "henning",
  "share-link-secret",
)

/** Texts an exception message or a log line could carry in a session with [SECRETS]. */
val LEAKY_TEXTS = listOf(
  "Failed to send \"meet me at the harbour at nine\" to Person { display_name: \"Alice Example\" }",
  "cannot open /storage/emulated/0/Download/quarterly-figures.xlsx",
  "auth failed: Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.c2lnbmF0dXJl",
  "sign-in refused password=s3cr3t-pa55word for alice@example.com",
  "upload to https://cdn.example.com/upload?X-Amz-Signature=abc123&X-Amz-Credential=x failed",
  "key material Q2hhbm5lbCBrZXkgbWF0ZXJpYWwgaGVyZQ rejected",
  "config at /home/henning/.config/teamcraft/app.toml unreadable",
  "share link https://team.example.com/s/1#share-link-secret expired",
)

class CrashScrubberTest {
  @Test
  fun aReportFromASessionWithSecretsContainsNoneOfThem() {
    val scrubbed = LEAKY_TEXTS.joinToString("\n") { CrashScrubber.scrub(it) }
    for (secret in SECRETS) assertFalse("$secret leaked into $scrubbed", scrubbed.contains(secret))
    // What helps fixing it is still there.
    assertTrue(scrubbed.contains("Failed to send"))
    assertTrue(scrubbed.contains("https://cdn.example.com/upload?[redacted]"))
  }

  @Test
  fun quotedValuesGoWhole() {
    assertEquals(
      "Person { display_name: \"[redacted]\", note: \"[redacted]\" }",
      CrashScrubber.removeQuoted("Person { display_name: \"Alice Example\", note: \"say \\\"hi\\\"\" }"),
    )
    assertEquals("open \"[redacted]", CrashScrubber.removeQuoted("open \"half"))
    assertEquals("no quotes here", CrashScrubber.removeQuoted("no quotes here"))
  }

  @Test
  fun idsRoutesAndWordsStay() {
    val line = "sync failed conversation_id=1234 at /api/v1/conversations/7346/messages: timeout"
    assertEquals(line, CrashScrubber.scrub(line))
  }

  @Test
  fun urlsKeepHostAndPathOnly() {
    assertEquals(
      "GET https://[redacted]@team.example.com/api/v1/x?[redacted].",
      CrashScrubber.redact("GET https://user:pw@team.example.com/api/v1/x?token=abc#frag."),
    )
    assertEquals("open [path]", CrashScrubber.redact("open content://media/external/images/1"))
  }

  @Test
  fun longTextsAreCut() {
    val scrubbed = CrashScrubber.scrub("x".repeat(2_000))
    assertEquals(CrashScrubber.MAX_MESSAGE_CHARS + 1, scrubbed.length)
    assertTrue(scrubbed.endsWith("…"))
  }

  @Test
  fun emptyTextStaysEmpty() {
    assertEquals("", CrashScrubber.scrub(""))
  }
}
