package de.zeitgleich.teamcraft.crash

import io.sentry.Breadcrumb
import io.sentry.SentryEvent
import io.sentry.SentryLevel
import io.sentry.protocol.Message
import io.sentry.protocol.User

/**
 * sentry-android's `beforeSend` (docs/crash-reports.md, "The Android SDK"): samples handled errors,
 * drops what a report never carries (request, server name, extras, every user field but the
 * installation ID), scrubs exception and message texts ([CrashScrubber]) and puts the core's
 * redacted log lines in as the only breadcrumbs.
 */
object SentryEventScrub {
  /** The event to send, or `null` when a handled error isn't in the sample. */
  fun prepare(
    event: SentryEvent,
    installationId: String,
    sampleRate: Double,
    roll: Double,
    logLines: () -> List<String>,
  ): SentryEvent? {
    if (!isCrash(event) && roll >= sampleRate) return null
    event.request = null
    event.serverName = null
    event.setExtras(null)
    event.user = User().apply { id = installationId }
    event.exceptions?.forEach { exception ->
      exception.value = exception.value?.let(CrashScrubber::scrub)
    }
    event.message = event.message?.let { message ->
      Message().apply { formatted = (message.formatted ?: message.message)?.let(CrashScrubber::scrub) }
    }
    event.logger = null
    event.breadcrumbs = logLines()
      .takeLast(CrashScrubber.MAX_BREADCRUMBS)
      .map { line ->
        Breadcrumb(CrashScrubber.scrub(line, CrashScrubber.MAX_BREADCRUMB_CHARS)).apply { category = "log" }
      }
      .toMutableList()
    return event
  }

  /** A crash or ANR (always sent), not a handled error (sampled). */
  private fun isCrash(event: SentryEvent): Boolean = event.level == SentryLevel.FATAL || event.isCrashed
}
