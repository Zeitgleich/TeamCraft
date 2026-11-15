package de.zeitgleich.teamcraft.ui

import org.junit.Assert.assertEquals
import org.junit.Test

class ReadingPlacesTest {
  @Test
  fun placesRoundTripThroughTheirStoredForm() {
    val places = listOf("10" to "500", "11" to "600")
    assertEquals(places, ReadingPlaces.decode(ReadingPlaces.encode(places)))
  }

  @Test
  fun damagedLinesAreSkipped() {
    assertEquals(listOf("1" to "2"), ReadingPlaces.decode("garbage\n=3\n4=\n1=2"))
    assertEquals(emptyList<Pair<String, String>>(), ReadingPlaces.decode(null))
  }

  @Test
  fun anOpenedFeedReturnsToWhereTheUserStoppedWhenNothingIsUnread() {
    val state = FeedScrollState()
    val placed = decideFeedScroll(state, FeedScrollEvent.Opened(null, null, maybePlaceIndex = 7), isNearBottom = true)
    assertEquals(FeedScrollAction.ShowAtTop(7), placed.action)
    val unread = decideFeedScroll(state, FeedScrollEvent.Opened(null, 3, maybePlaceIndex = 7), isNearBottom = true)
    assertEquals(FeedScrollAction.ShowAtTop(3), unread.action)
  }
}
