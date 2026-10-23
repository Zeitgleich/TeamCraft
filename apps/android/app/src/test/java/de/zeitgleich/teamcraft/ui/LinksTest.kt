package de.zeitgleich.teamcraft.ui

import org.junit.Assert.assertEquals
import org.junit.Test

class LinksTest {
  @Test
  fun findsWebLinksAndDropsSentencePunctuation() {
    val text = "Slides: https://example.com/slides. Also see http://example.com/a?b=c, thanks!"
    assertEquals(listOf("https://example.com/slides", "http://example.com/a?b=c"), links(text))
  }

  @Test
  fun keepsBalancedParenthesesOnly() {
    assertEquals(listOf("https://example.com/wiki/A_(b)"), links("(see https://example.com/wiki/A_(b))"))
  }

  @Test
  fun ignoresTextWithoutScheme() {
    assertEquals(emptyList<String>(), links("example.com and www.example.com are not links here"))
    assertEquals(emptyList<String>(), links(""))
  }

  private fun links(text: String) = findLinks(text).map { text.substring(it) }
}
