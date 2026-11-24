package de.zeitgleich.teamcraft.ui

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class TextPreviewRequestTest {
  @Test
  fun textAndDelimitedFilesOpenInThePreview() {
    assertTrue(isTextPreviewable("notes.txt", "text/plain"))
    assertTrue(isTextPreviewable("sheet.csv", "text/csv"))
    assertTrue(isTextPreviewable("sheet.tsv", "text/tab-separated-values; charset=utf-8"))
    assertTrue(isTextPreviewable("export.csv", "application/vnd.ms-excel"))
  }

  @Test
  fun aGenericTypeIsJudgedByTheName() {
    assertTrue(isTextPreviewable("data.CSV", "application/octet-stream"))
    assertTrue(isTextPreviewable("server.log", ""))
    assertFalse(isTextPreviewable("photo.jpg", "application/octet-stream"))
  }

  @Test
  fun otherFilesGoToOtherApps() {
    assertFalse(isTextPreviewable("report.pdf", "application/pdf"))
    assertFalse(isTextPreviewable("budget.xlsx", "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"))
    assertFalse(isTextPreviewable("old.xls", "application/vnd.ms-excel"))
  }

  @Test
  fun choicesAreKeptPerTypeOfFile() {
    assertEquals("csv", textPreviewTypeKey("Sheet.CSV", "text/csv"))
    assertEquals("text/plain", textPreviewTypeKey("README", "text/plain; charset=utf-8"))
  }
}
