package de.zeitgleich.teamcraft.ui

import java.time.LocalDate
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class SearchFiltersTest {
  @Test
  fun tokensQuoteValuesWithSpaces() {
    assertEquals("from:alice", SearchFilters.from("alice"))
    assertEquals("in:\"project a\"", SearchFilters.inConversation("project a"))
    assertEquals("in:general", SearchFilters.inConversation("#general"))
    assertEquals("in:\"say hi\"", SearchFilters.inConversation("say \"hi\""))
    assertNull(SearchFilters.from("  "))
  }

  @Test
  fun datesUseIsoDays() {
    assertEquals("after:2026-03-01", SearchFilters.after(LocalDate.of(2026, 3, 1)))
    assertEquals("before:2026-12-31", SearchFilters.before(LocalDate.of(2026, 12, 31)))
    assertEquals(LocalDate.of(1970, 1, 2), SearchFilters.dayOf(86_400_000L))
  }

  @Test
  fun addAppendsAndReplacesSingleUseKeys() {
    assertEquals("report from:alice", SearchFilters.add("report", "from:alice"))
    assertEquals("report from:bob", SearchFilters.add("report from:alice", "from:bob"))
    assertEquals("has:file has:link", SearchFilters.add("has:file", "has:link"))
    assertEquals("has:file", SearchFilters.add("has:file", "has:file"))
    assertEquals("in:\"project a\"", SearchFilters.add("", "in:\"project a\""))
  }

  @Test
  fun removeDropsTheWholeToken() {
    assertEquals("report has:link", SearchFilters.remove("report in:\"project a\" has:link", "in:\"project a\""))
    assertEquals("", SearchFilters.remove("has:file", "has:file"))
  }

  @Test
  fun activeListsOnlyFilters() {
    assertEquals(
      listOf("from:alice", "in:\"project a\"", "in-thread"),
      SearchFilters.active("\"quarter: plan\" from:alice budget in:\"project a\" in-thread"),
    )
    assertTrue(SearchFilters.contains("x has:file", "has:file"))
    assertFalse(SearchFilters.contains("x has:files", "has:file"))
  }
}
