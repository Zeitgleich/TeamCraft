package de.zeitgleich.teamcraft.calls

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class CallAloneTest {
  @Test
  fun aDirectCallEndsOnceTheOtherPersonLeft() {
    assertTrue(isLeftAlone(isOneToOne = true, hadOthers = true, participantCount = 0, reconnectingCount = 0))
  }

  @Test
  fun aCallStaysWhileTheOtherPersonIsOnTheirWayInOrComingBack() {
    assertFalse(isLeftAlone(isOneToOne = true, hadOthers = false, participantCount = 0, reconnectingCount = 0))
    assertFalse(isLeftAlone(isOneToOne = true, hadOthers = true, participantCount = 0, reconnectingCount = 1))
    assertFalse(isLeftAlone(isOneToOne = true, hadOthers = true, participantCount = 1, reconnectingCount = 0))
  }

  @Test
  fun groupCallsStayOpenForWhoeverComes() {
    assertFalse(isLeftAlone(isOneToOne = false, hadOthers = true, participantCount = 0, reconnectingCount = 0))
  }
}
