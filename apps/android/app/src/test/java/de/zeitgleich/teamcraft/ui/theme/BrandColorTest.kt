package de.zeitgleich.teamcraft.ui.theme

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class BrandColorTest {
  @Test
  fun longAndShortHexBecomeOpaqueArgb() {
    assertEquals(0xFF0B7F78L, brandArgb("#0b7f78"))
    assertEquals(0xFFFFAA00L, brandArgb("#fa0"))
  }

  @Test
  fun anythingElseIsRejected() {
    for (value in listOf("", "#", "0b7f78", "#0b7f7", "#0b7f7800", "#ggg", "red", "url(x)", "#-12")) {
      assertNull(value, brandArgb(value))
    }
  }
}
