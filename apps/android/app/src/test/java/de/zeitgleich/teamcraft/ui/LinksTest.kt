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

  @Test
  fun messageLinksAndMentionsAreSpans() {
    val text = "@alice see teamcraft:/c/12/m/34/t/56 and @bob, @here"
    val spans = textSpans(text, usernames = setOf("alice", "carol"), myUsername = "Bob")
    assertEquals(
      listOf(
        TextSpan.Mention(0..5, isMe = false),
        TextSpan.Permalink(11..35, "teamcraft:/c/12/m/34/t/56"),
        TextSpan.Mention(41..44, isMe = true),
        TextSpan.Mention(47..51, isMe = true),
      ),
      spans,
    )
  }

  @Test
  fun unknownNamesAndAddressesAreNoMentions() {
    assertEquals(emptyList<TextSpan>(), textSpans("mail dave@example.com or @nobody", setOf("dave"), "me"))
  }

  @Test
  fun mentionsInsideLinksAreSkipped() {
    val text = "https://example.com/@alice"
    assertEquals(listOf(TextSpan.WebLink(0..25, text)), textSpans(text, setOf("alice"), "me"))
  }

  @Test
  fun malformedMessageLinksAreNotLinks() {
    assertEquals(emptyList<TextSpan>(), textSpans("teamcraft:/c/abc/m/1 teamcraft:/c/1", emptySet(), "me"))
  }

  private fun links(text: String) = findLinks(text).map { text.substring(it) }
}
