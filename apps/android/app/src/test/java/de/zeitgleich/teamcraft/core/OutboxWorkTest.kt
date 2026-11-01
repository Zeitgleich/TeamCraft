package de.zeitgleich.teamcraft.core

import org.junit.Assert.assertEquals
import org.junit.Test
import uniffi.teamcraft_ffi.PendingWorkItem

class OutboxWorkTest {
  @Test
  fun aRunThatSentEverythingIsDone() {
    assertEquals(OutboxWork.Outcome.Done, OutboxWork.outcome(EMPTY, attempt = 0))
  }

  @Test
  fun nothingToSendForIsDone() {
    assertEquals("signed out or not set up", OutboxWork.Outcome.Done, OutboxWork.outcome(null, attempt = 0))
  }

  @Test
  fun whatIsLeftIsRetriedWithBackoff() {
    val left = PendingWorkItem(sends = 1u, uploads = 1u, actions = 0u, isEmpty = false)
    assertEquals(OutboxWork.Outcome.Retry, OutboxWork.outcome(left, attempt = 0))
    assertEquals(OutboxWork.Outcome.Retry, OutboxWork.outcome(left, attempt = OutboxWork.MAX_ATTEMPTS - 2))
  }

  @Test
  fun afterTheLastAttemptTheNextForegroundSendsTheRest() {
    val left = PendingWorkItem(sends = 0u, uploads = 0u, actions = 2u, isEmpty = false)
    assertEquals(OutboxWork.Outcome.Done, OutboxWork.outcome(left, attempt = OutboxWork.MAX_ATTEMPTS - 1))
  }

  private companion object {
    val EMPTY = PendingWorkItem(sends = 0u, uploads = 0u, actions = 0u, isEmpty = true)
  }
}
