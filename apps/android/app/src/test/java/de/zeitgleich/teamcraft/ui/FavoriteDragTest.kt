package de.zeitgleich.teamcraft.ui

import de.zeitgleich.teamcraft.ui.screens.FavoriteDragState
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Test

class FavoriteDragTest {
  @Test
  fun aDragPastHalfARowMovesOnePlaceAndGivesTheRowBack() {
    val state = FavoriteDragState().apply { height = 100 }
    val moves = mutableListOf<Boolean>()
    state.dragBy(40f) { moves += it; true }
    assertEquals(emptyList<Boolean>(), moves)
    state.dragBy(20f) { moves += it; true }
    assertEquals(listOf(false), moves)
    assertEquals(-40f, state.offset, 0.001f)
  }

  @Test
  fun aDragUpMovesUp() {
    val state = FavoriteDragState().apply { height = 100 }
    val moves = mutableListOf<Boolean>()
    state.dragBy(-230f) { moves += it; true }
    assertEquals(listOf(true, true), moves)
    assertEquals(-30f, state.offset, 0.001f)
  }

  @Test
  fun atTheEndTheRowStopsHalfARowOff() {
    val state = FavoriteDragState().apply { height = 100 }
    state.dragBy(300f) { false }
    assertEquals(50f, state.offset, 0.001f)
  }

  @Test
  fun endingADragPutsTheRowBack() {
    val state = FavoriteDragState().apply { height = 100; isDragging = true; offset = 30f }
    state.end()
    assertFalse(state.isDragging)
    assertEquals(0f, state.offset, 0.001f)
  }

  @Test
  fun linkAnnotationTagsNameTheirKind() {
    assertEquals(DetectedKind.WEB, detectedKindOfUri("https://example.com/a"))
    assertEquals(DetectedKind.PHONE, detectedKindOfUri("tel:+4912345"))
    assertEquals(DetectedKind.EMAIL, detectedKindOfUri("mailto:a@example.com"))
    assertEquals(DetectedKind.ADDRESS, detectedKindOfUri("geo:0,0?q=x"))
    assertEquals(null, detectedKindOfUri("teamcraft:/c/1/m/2"))
  }
}
