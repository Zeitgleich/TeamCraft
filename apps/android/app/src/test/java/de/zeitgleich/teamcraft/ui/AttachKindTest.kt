package de.zeitgleich.teamcraft.ui

import org.junit.Assert.assertEquals
import org.junit.Test

class AttachKindTest {
  @Test
  fun theLastUsedKindComesFirst() {
    assertEquals(listOf(AttachKind.FILE, AttachKind.CAMERA, AttachKind.GALLERY), AttachKind.ordered(AttachKind.FILE))
    assertEquals(AttachKind.entries, AttachKind.ordered(AttachKind.CAMERA))
  }

  @Test
  fun anUnknownStoredNameFallsBackToTheCamera() {
    assertEquals(AttachKind.CAMERA, AttachKind.parse(null))
    assertEquals(AttachKind.CAMERA, AttachKind.parse("nonsense"))
    assertEquals(AttachKind.GALLERY, AttachKind.parse("GALLERY"))
  }
}
