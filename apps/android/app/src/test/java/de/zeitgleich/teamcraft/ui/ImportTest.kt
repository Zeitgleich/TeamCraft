package de.zeitgleich.teamcraft.ui

import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class ImportTest {
  @Test
  fun copiesUpToTheLimit() {
    val output = ByteArrayOutputStream()
    assertTrue(copyAtMost(ByteArrayInputStream(ByteArray(100)), output, 100))
    assertEquals(100, output.size())
  }

  @Test
  fun stopsOverTheLimit() {
    assertFalse(copyAtMost(ByteArrayInputStream(ByteArray(101)), ByteArrayOutputStream(), 100))
  }

  @Test
  fun dropsControlAndDirectionCharactersFromNames() {
    // "invoice‮fdp.exe" shows as "invoiceexe.pdf".
    assertEquals("invoicefdp.exe", cleanFileName("invoice‮fdp.exe"))
    assertEquals("a b.txt", cleanFileName(" a\n b.txt\u0000 "))
  }
}
