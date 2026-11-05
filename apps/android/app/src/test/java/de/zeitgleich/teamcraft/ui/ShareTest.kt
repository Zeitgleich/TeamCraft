package de.zeitgleich.teamcraft.ui

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class ShareTest {
  @Test
  fun aPagesTitleGoesAboveItsAddress() {
    assertEquals("A page\nhttps://example.com/a", sharedText("A page", "https://example.com/a"))
    assertEquals("https://example.com/a", sharedText(null, " https://example.com/a "))
    assertEquals("https://example.com/a", sharedText("https://example.com/a", "https://example.com/a"))
  }

  @Test
  fun aSubjectNextToOtherTextStaysOut() {
    assertEquals("Look at https://example.com/a", sharedText("Re: hello", "Look at https://example.com/a"))
    assertEquals("", sharedText("Only a subject", ""))
  }

  @Test
  fun sharedTextIsBounded() {
    assertEquals(MAX_SHARED_TEXT_CHARS, sharedText(null, "x".repeat(MAX_SHARED_TEXT_CHARS + 5)).length)
  }

  @Test
  fun acceptsOtherAppsContent() {
    assertTrue(isForeignContent("content", "com.example.gallery.provider", PACKAGE))
    assertTrue(isForeignContent("CONTENT", "media", PACKAGE))
  }

  @Test
  fun rejectsFilesAndTheAppsOwnProviders() {
    // A hostile app could point at the app's private store and have the user send it.
    assertFalse(isForeignContent("file", null, PACKAGE))
    assertFalse(isForeignContent("file", "", PACKAGE))
    assertFalse(isForeignContent("content", "$PACKAGE.files", PACKAGE))
    assertFalse(isForeignContent("content", PACKAGE, PACKAGE))
    assertFalse(isForeignContent("content", null, PACKAGE))
    assertFalse(isForeignContent(null, "com.example.gallery.provider", PACKAGE))
  }

  @Test
  fun rejectsUserPrefixedAuthorities() {
    // The resolver strips `0@`, so this would open the app's own provider.
    assertFalse(isForeignContent("content", "0@$PACKAGE.files", PACKAGE))
    assertFalse(isForeignContent("content", "10@com.example.gallery.provider", PACKAGE))
  }

  @Test
  fun similarlyNamedProvidersOfOtherAppsAreForeign() {
    assertTrue(isForeignContent("content", "${PACKAGE}x.files", PACKAGE))
  }

  private companion object {
    const val PACKAGE = "de.zeitgleich.teamcraft"
  }
}
