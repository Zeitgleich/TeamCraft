package de.zeitgleich.teamcraft.ui

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class FavoriteOrderTest {
  @Test
  fun aFavoriteMovesOnePlace() {
    val shown = listOf("a", "b", "c")
    assertEquals(listOf("b", "a", "c"), FavoriteOrder.moved(shown, "b", up = true))
    assertEquals(listOf("a", "c", "b"), FavoriteOrder.moved(shown, "b", up = false))
  }

  @Test
  fun noMoveOffTheEndsOrForAnUnknownOne() {
    val shown = listOf("a", "b")
    assertNull(FavoriteOrder.moved(shown, "a", up = true))
    assertNull(FavoriteOrder.moved(shown, "b", up = false))
    assertNull(FavoriteOrder.moved(shown, "x", up = true))
  }
}
