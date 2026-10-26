package de.zeitgleich.teamcraft.ui

import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.LinkAnnotation
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextDecoration
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.teamcraft_ffi.MarkupLine
import uniffi.teamcraft_ffi.MarkupRun
import uniffi.teamcraft_ffi.MarkupRunKind

class MessageMarkupTest {
  @Test
  fun inlineCodeShowsWithoutBackticksInMonospace() {
    val text = annotate(text("run "), run(MarkupRunKind.Code, "curl -v"))
    assertEquals("run  curl -v ", text.text)
    assertTrue(text.styleAt("curl").any { it.fontFamily == FontFamily.Monospace && it.background == CODE })
  }

  @Test
  fun emphasisBecomesSpans() {
    val text = annotate(run(MarkupRunKind.Text, "loud", isBold = true, isItalic = true), run(MarkupRunKind.Text, "gone", isStrike = true, isUnderline = true))
    assertTrue(text.styleAt("loud").any { it.fontWeight == FontWeight.Bold })
    assertTrue(text.styleAt("loud").any { it.fontStyle == FontStyle.Italic })
    val decoration = text.styleAt("gone").mapNotNull { it.textDecoration }.single()
    assertTrue(decoration.contains(TextDecoration.LineThrough) && decoration.contains(TextDecoration.Underline))
  }

  @Test
  fun aLongBareLinkIsShortenedButKeepsItsTarget() {
    val url = "https://docs.example.com/projects/project-a/reports/2026/october/summary.pdf"
    val text = annotate(link(url, url, isLabeled = false))
    assertEquals("docs.example.com/…/summary.pdf", text.text)
    assertEquals(listOf(url), text.linkTags())
  }

  @Test
  fun aLabeledLinkShowsItsLabel() {
    val text = annotate(link("https://example.com/a", "the docs", isLabeled = true))
    assertEquals("the docs", text.text)
    assertEquals(listOf("https://example.com/a"), text.linkTags())
  }

  @Test
  fun aLinkTheSharedRuleRefusesStaysText() {
    val text = annotate(link("http://intranet.example.com", "http://intranet.example.com", isLabeled = false), isOpenable = { false })
    assertEquals("http://intranet.example.com", text.text)
    assertEquals(emptyList<String>(), text.linkTags())
  }

  @Test
  fun aPermalinkReadsAsAMessageLink() {
    val permalink = "teamcraft:/c/1/m/2"
    val text = annotate(link(permalink, permalink, isLabeled = false))
    assertEquals(PERMALINK_LABEL, text.text)
    assertEquals(listOf(permalink), text.linkTags())
  }

  @Test
  fun onlyKnownNamesAreHighlighted() {
    val text = annotate(mention("alice"), text(" "), mention("me"), text(" "), mention("nobody"), text(" "), mention("channel"))
    assertTrue(text.styleAt("@alice").any { it.color == MENTION && it.fontWeight == FontWeight.Bold })
    assertTrue(text.styleAt("@me").any { it.background == MENTION_ME_BACKGROUND })
    assertTrue(text.styleAt("@channel").any { it.background == MENTION_ME_BACKGROUND })
    assertTrue(text.styleAt("@nobody").none { it.fontWeight == FontWeight.Bold })
  }

  @Test
  fun linesOfAParagraphAreSeparatedByLineBreaks() {
    val text = annotatedLines(
      listOf(MarkupLine(listOf(text("one"))), MarkupLine(listOf(text("two")))),
      COLORS,
      NAMES,
      handlers(),
      PERMALINK_LABEL,
    )
    assertEquals("one\ntwo", text.text)
  }

  @Test
  fun shortLinksStayWholeWithoutTheScheme() {
    assertEquals("example.com/a", shortLinkLabel("https://www.example.com/a/"))
    assertEquals("example.com", shortLinkLabel("http://example.com"))
  }

  @Test
  fun aLongLinkWithoutAPathIsCut() {
    val label = shortLinkLabel("https://" + "a".repeat(60) + ".example.com", maxChars = 20)
    assertEquals(20, label.length)
    assertTrue(label.endsWith("…"))
  }

  @Test
  fun queryAndFragmentDoNotCountAsTheLastSegment() {
    assertEquals(
      "example.com/…/page",
      shortLinkLabel("https://example.com/a/very/long/path/to/the/page?utm_source=x&utm_medium=y#top", maxChars = 30),
    )
  }

  private fun annotate(vararg runs: MarkupRun, isOpenable: (String) -> Boolean = { true }): AnnotatedString =
    annotatedLine(MarkupLine(runs.toList()), COLORS, NAMES, handlers(isOpenable), PERMALINK_LABEL)

  private fun handlers(isOpenable: (String) -> Boolean = { true }) = LinkHandlers(isOpenable, onOpenWeb = {}, onOpenPermalink = {})

  private fun AnnotatedString.styleAt(part: String) = run {
    val start = text.indexOf(part)
    spanStyles.filter { it.start <= start && start + part.length <= it.end }.map { it.item }
  }

  private fun AnnotatedString.linkTags(): List<String> =
    getLinkAnnotations(0, length).map { (it.item as LinkAnnotation.Clickable).tag }

  private fun text(text: String) = run(MarkupRunKind.Text, text)

  private fun mention(name: String) = run(MarkupRunKind.Mention(name), "@$name")

  private fun link(url: String, text: String, isLabeled: Boolean) = run(MarkupRunKind.Link(url, isLabeled), text)

  private fun run(kind: MarkupRunKind, text: String, isBold: Boolean = false, isItalic: Boolean = false, isUnderline: Boolean = false, isStrike: Boolean = false) =
    MarkupRun(kind = kind, text = text, isBold = isBold, isItalic = isItalic, isUnderline = isUnderline, isStrike = isStrike)

  private companion object {
    const val PERMALINK_LABEL = "Message link"
    val MENTION = Color(0xFF005DFC)
    val MENTION_ME_BACKGROUND = Color(0xFFFFF2CC)
    val CODE = Color(0x14000000)
    val COLORS = MarkupColors(link = MENTION, mention = MENTION, mentionMe = Color(0xFF6A4B00), mentionMeBackground = MENTION_ME_BACKGROUND, codeBackground = CODE)
    val NAMES = MentionNames(usernames = setOf("alice", "me"), myUsername = "me")
  }
}
