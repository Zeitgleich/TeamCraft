package de.zeitgleich.teamcraft.calls

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class IncomingRingTest {
  @Test
  fun aDismissOfThisCallEndsTheRing() {
    assertTrue(ring(maybeCallId = "7").isEndedBy("7"))
  }

  @Test
  fun aLateDismissOfAnotherCallDoesNotSilenceANewRing() {
    assertFalse(ring(maybeCallId = "8").isEndedBy("7"))
  }

  @Test
  fun aGatewayRingIsNotEndedByAPushDismiss() {
    assertFalse(ring(maybeCallId = null).isEndedBy("7"))
  }

  @Test
  fun aPushForTheRingingConversationAddsTheCallId() {
    assertEquals("7", ring(maybeCallId = null).merged(ring(maybeCallId = "7"))?.maybeCallId)
  }

  @Test
  fun aRingOfAnotherConversationDoesNotMerge() {
    assertNull(ring(maybeCallId = null).merged(ring(maybeCallId = "7", conversationId = "2")))
  }

  private fun ring(maybeCallId: String?, conversationId: String = "1") =
    IncomingRing(conversationId, maybeCallId, displayName = "planning", maybeCallerName = "Alice")
}
