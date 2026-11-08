package de.zeitgleich.teamcraft.crash

import io.sentry.protocol.SentrySpan
import io.sentry.protocol.SentryTransaction
import io.sentry.protocol.User

/**
 * sentry-android's `beforeSendTransaction` (docs/crash-reports.md, "Performance data"): performance
 * data carries what a crash report may carry and nothing more. Request, server name, extras and
 * breadcrumbs go, the user is only the installation ID, the transaction's tag values and span
 * descriptions pass [CrashScrubber] (a URL keeps scheme, host and path; its query and fragment go),
 * and span data and tags (which can hold full URLs, queries and file paths) are dropped.
 */
object SentryTransactionScrub {
  /** The transaction to send, scrubbed in place. */
  fun prepare(transaction: SentryTransaction, installationId: String): SentryTransaction {
    transaction.request = null
    transaction.serverName = null
    transaction.setExtras(null)
    transaction.breadcrumbs = null
    transaction.user = User().apply { id = installationId }
    transaction.tags?.let { tags ->
      for (key in tags.keys.toList()) transaction.setTag(key, CrashScrubber.redact(tags.getValue(key)))
    }
    val spans = transaction.spans
    val scrubbed = spans.map(::scrubSpan)
    spans.clear()
    spans.addAll(scrubbed)
    return transaction
  }

  private fun scrubSpan(span: SentrySpan): SentrySpan =
    SentrySpan(
      span.startTimestamp,
      span.timestamp,
      span.traceId,
      span.spanId,
      span.parentSpanId,
      span.op,
      span.description?.let { CrashScrubber.scrub(it, MAX_DESCRIPTION_CHARS) },
      span.status,
      span.origin,
      // A span's tags are the SDK's (an HTTP call's query among them): none are needed.
      emptyMap(),
      span.measurements,
      // Span data holds the full URL, query and body sizes of HTTP calls, file paths of file I/O.
      null,
    )

  private const val MAX_DESCRIPTION_CHARS = 256
}
