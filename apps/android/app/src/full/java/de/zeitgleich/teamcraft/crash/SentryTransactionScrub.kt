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
  /**
   * Whether [transaction] is a screen load (`ui.load`) nobody saw (#1641): its time to initial
   * display waited longer than [StartVisibility] allows for the first frame, because the activity
   * was created while not visible (screen off, locked, started or restored in the background). Its
   * duration is the time until the user came back, not a slow screen: it is not sent.
   */
  fun isUnseenScreenLoad(transaction: SentryTransaction): Boolean {
    if (transaction.contexts.trace?.operation != SCREEN_LOAD_OP) return false
    val initialDisplay = transaction.spans.firstOrNull { it.op == INITIAL_DISPLAY_OP } ?: return false
    val endSeconds = initialDisplay.timestamp ?: return false
    return (endSeconds - initialDisplay.startTimestamp) * 1000.0 > StartVisibility.FIRST_FRAME_SEEN_WITHIN_MS
  }

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
      // Span data holds the full URL, query and body sizes of HTTP calls, file paths of file I/O:
      // only the core jobs' queue facts (a number and a static name) stay.
      span.data?.filterKeys { it in KEPT_DATA }?.filterValues { it is Number || (it is String && it in KEPT_QUEUES) },
    )

  private const val SCREEN_LOAD_OP = "ui.load"
  private const val INITIAL_DISPLAY_OP = "ui.load.initial_display"
  private const val MAX_DESCRIPTION_CHARS = 256
  private val KEPT_DATA = setOf("teamcraft.queue_depth", "teamcraft.queue")
  private val KEPT_QUEUES = setOf("core.read", "core.engine", "store.lock_wait", "store.write_group", "core.step")
}
