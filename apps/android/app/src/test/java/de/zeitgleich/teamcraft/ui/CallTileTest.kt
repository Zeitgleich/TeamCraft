package de.zeitgleich.teamcraft.ui

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import de.zeitgleich.teamcraft.core.Message
import de.zeitgleich.teamcraft.core.WireJson
import de.zeitgleich.teamcraft.ui.screens.callDuration

/** The core's call tiles (docs/notifications.md, "Calls in the timeline") as the app reads them. */
class CallTileTest {
  @Test
  fun anOngoingCallIsReadFromTheMessageKind() {
    val call = message("""{"Call":{"call":"7","state":{"state":"ongoing"}}}""").maybeCall!!
    assertEquals("7", call.call)
    assertTrue(call.state.isOngoing)
    assertFalse(call.isMissed)
  }

  @Test
  fun anEndedCallCarriesItsDuration() {
    val call = message("""{"Call":{"call":"7","state":{"state":"ended","duration_ms":185000},"is_missed":true}}""").maybeCall!!
    assertFalse(call.state.isOngoing)
    assertEquals(185_000L, call.state.maybeDurationMs)
    assertTrue(call.isMissed)
  }

  @Test
  fun otherKindsAreNoCall() {
    assertNull(message("\"User\"").maybeCall)
    assertNull(message("""{"System":"Joined"}""").maybeCall)
  }

  @Test
  fun callTilesStandAloneInTheFeed() {
    val tile = message("""{"Call":{"call":"7","state":{"state":"ongoing"}}}""", id = "2")
    val items = feedItems(listOf(message("\"User\"", id = "1"), tile, message("\"User\"", id = "3")), maybeFirstUnread = null)
    val entries = items.filterIsInstance<FeedItem.Entry>()
    assertEquals(FeedContentType.Event, entries[1].contentType)
    assertFalse(entries[2].isContinuation)
  }

  @Test
  fun durationsReadAsClockTimes() {
    assertEquals("0:00", callDuration(0))
    assertEquals("3:05", callDuration(185_000))
    assertEquals("1:02:03", callDuration(3_723_000))
    assertEquals("0:00", callDuration(-5))
  }

  private fun message(kind: String, id: String = "1"): Message = WireJson.decodeFromString(
    Message.serializer(),
    """{"id":"$id","conversation":"9","author":"3","kind":$kind,"sent_at":1800000000000,"body":""}""",
  )
}
