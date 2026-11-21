package de.zeitgleich.teamcraft.ui

import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream
import java.io.File
import java.io.IOException
import java.io.InputStream
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

  @Test
  fun stagesACompleteCopy() {
    val target = File.createTempFile("paste", ".bin")
    assertTrue(copyToStaging({ ByteArrayInputStream(ByteArray(50) { 7 }) }, target, 100))
    assertEquals(50, target.length())
    target.delete()
  }

  @Test
  fun leavesNoHalfFileOverTheLimitOnCancelOrFailure() {
    val over = File.createTempFile("paste", ".bin")
    assertFalse(copyToStaging({ ByteArrayInputStream(ByteArray(101)) }, over, 100))
    assertFalse(over.exists())
    val cancelled = File.createTempFile("paste", ".bin")
    assertFalse(copyToStaging({ ByteArrayInputStream(ByteArray(10)) }, cancelled, 100, isCancelled = { true }))
    assertFalse(cancelled.exists())
    val broken = File.createTempFile("paste", ".bin")
    val failing = object : InputStream() {
      override fun read(): Int = throw IOException("gone")
    }
    assertFalse(copyToStaging({ failing }, broken, 100))
    assertFalse(broken.exists())
    val missing = File.createTempFile("paste", ".bin")
    assertFalse(copyToStaging({ null }, missing, 100))
    assertFalse(missing.exists())
  }

  @Test
  fun refusesADeclaredSizeOverTheLimitBeforeCopying() {
    assertTrue(isWithinImportLimit(100, 100))
    assertFalse(isWithinImportLimit(101, 100))
    assertTrue(isWithinImportLimit(-1, 100))
  }
}
