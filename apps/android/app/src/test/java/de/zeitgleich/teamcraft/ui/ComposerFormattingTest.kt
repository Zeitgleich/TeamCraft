package de.zeitgleich.teamcraft.ui

import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.TextRange
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.OffsetMapping
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.ui.text.AnnotatedString
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.teamcraft_ffi.ComposerEdit
import uniffi.teamcraft_ffi.ComposerSpan
import uniffi.teamcraft_ffi.ComposerSpanKind

class ComposerFormattingTest {
  private val colors = ComposerStyleColors(
    marker = Color.Gray, quote = Color.DarkGray, link = Color.Blue, mention = Color.Cyan, codeBackground = Color.LightGray,
  )

  private fun span(start: Int, end: Int, kind: ComposerSpanKind) = ComposerSpan(start.toUInt(), end.toUInt(), kind)

  private fun edit(text: String, at: Int) = ComposerEdit(text, at.toUInt(), at.toUInt())

  @Test
  fun spansStyleWithoutChangingTheText() {
    val styled = styledComposerText("a **b** c", listOf(span(4, 5, ComposerSpanKind.BOLD), span(2, 4, ComposerSpanKind.MARKER)), colors)
    assertEquals("a **b** c", styled.text)
    assertTrue(styled.spanStyles.any { it.start == 4 && it.end == 5 && it.item.fontWeight == FontWeight.Bold })
    assertTrue(styled.spanStyles.any { it.start == 2 && it.end == 4 && it.item.color == Color.Gray })
  }

  @Test
  fun codeAndItalicGetTheirLook() {
    val styled = styledComposerText("`x` _y_", listOf(span(1, 2, ComposerSpanKind.CODE), span(5, 6, ComposerSpanKind.ITALIC)), colors)
    assertTrue(styled.spanStyles.any { it.item.fontFamily == FontFamily.Monospace && it.item.background == Color.LightGray })
    assertTrue(styled.spanStyles.any { it.item.fontStyle == FontStyle.Italic })
  }

  @Test
  fun spansPastTheEndAreClamped() {
    val styled = styledComposerText("ab", listOf(span(1, 9, ComposerSpanKind.BOLD), span(7, 9, ComposerSpanKind.BOLD)), colors)
    assertEquals(1, styled.spanStyles.size)
    assertEquals(2, styled.spanStyles[0].end)
  }

  @Test
  fun theTransformationMapsEveryOffsetToItself() {
    val styled = styledComposerText("a **b**", listOf(span(4, 5, ComposerSpanKind.BOLD)), colors)
    val shown = ComposerStyleTransformation(styled).filter(AnnotatedString("a **b**"))
    assertEquals("a **b**", shown.text.text)
    assertEquals(3, shown.offsetMapping.originalToTransformed(3))
    assertEquals(3, shown.offsetMapping.transformedToOriginal(3))
    assertSame(OffsetMapping.Identity, shown.offsetMapping)
  }

  @Test
  fun aTextOtherThanTheStyledOneStaysAsItIs() {
    val styled = styledComposerText("old", emptyList(), colors)
    val typed = AnnotatedString("older")
    assertSame(typed, ComposerStyleTransformation(styled).filter(typed).text)
  }

  @Test
  fun enterIsPassedToTheCoreWithTheOldTextAndCaret() {
    val old = TextFieldValue("- one", TextRange(5))
    val new = TextFieldValue("- one\n", TextRange(6))
    var seen: Pair<String, Int>? = null
    val result = interceptKey(old, new, enter = { text, at -> seen = text to at; edit("- one\n- ", 8) })
    assertEquals("- one" to 5, seen)
    assertEquals(TextFieldValue("- one\n- ", TextRange(8)), result)
  }

  @Test
  fun enterInTheMiddleOfTheText() {
    val old = TextFieldValue("abcd", TextRange(2))
    val new = TextFieldValue("ab\ncd", TextRange(3))
    var seen: Pair<String, Int>? = null
    interceptKey(old, new, enter = { text, at -> seen = text to at; null })
    assertEquals("abcd" to 2, seen)
  }

  @Test
  fun backspaceIsPassedToTheCore() {
    val old = TextFieldValue("- ", TextRange(2))
    val new = TextFieldValue("-", TextRange(1))
    val result = interceptKey(old, new, backspace = { _, _ -> edit("", 0) })
    assertEquals(TextFieldValue("", TextRange(0)), result)
  }

  @Test
  fun aKeyTheCoreLeavesAloneStaysAsTyped() {
    val old = TextFieldValue("plain", TextRange(5))
    val new = TextFieldValue("plain\n", TextRange(6))
    assertNull(interceptKey(old, new, enter = { _, _ -> null }))
  }

  @Test
  fun nothingIsInterceptedWhileTheInputMethodComposes() {
    val composing = TextFieldValue("- に", TextRange(3), composition = TextRange(2, 3))
    val next = TextFieldValue("- に\n", TextRange(4))
    val fail: (String, Int) -> ComposerEdit? = { _, _ -> throw AssertionError("must not be asked") }
    assertNull(interceptKey(composing, next, enter = fail, backspace = fail))
    val old = TextFieldValue("- a", TextRange(3))
    val composingNew = TextFieldValue("- a\n", TextRange(4), composition = TextRange(3, 4))
    assertNull(interceptKey(old, composingNew, enter = fail, backspace = fail))
  }

  @Test
  fun pastesAndSelectionsAreNoKeys() {
    val fail: (String, Int) -> ComposerEdit? = { _, _ -> throw AssertionError("must not be asked") }
    // A pasted line break is more than a key press with a caret.
    assertNull(interceptKey(TextFieldValue("a", TextRange(1)), TextFieldValue("a\nb\n", TextRange(4)), enter = fail, backspace = fail))
    // A selection replaced by a line break.
    assertNull(interceptKey(TextFieldValue("abc", TextRange(0, 3)), TextFieldValue("\n", TextRange(1)), enter = fail, backspace = fail))
    // Typing a letter.
    assertNull(interceptKey(TextFieldValue("a", TextRange(1)), TextFieldValue("ab", TextRange(2)), enter = fail, backspace = fail))
  }
}
