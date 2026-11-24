package de.zeitgleich.teamcraft.core

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class QueuedSinceForegroundTest {
  @Test
  fun aSendSchedulesTheNextLeaveAndOnlyThatOne() {
    val queued = QueuedSinceForeground()
    assertFalse("nothing sent: no run", queued.take())
    queued.note()
    queued.note()
    assertTrue(queued.take())
    assertFalse("handed over already", queued.take())
  }
}
