package de.zeitgleich.teamcraft.ui

import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.TextRange
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.OffsetMapping
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.ui.text.input.TransformedText
import androidx.compose.ui.text.input.VisualTransformation
import androidx.compose.ui.text.style.TextDecoration
import uniffi.teamcraft_ffi.ComposerEdit
import uniffi.teamcraft_ffi.ComposerFormat
import uniffi.teamcraft_ffi.ComposerSpan
import uniffi.teamcraft_ffi.ComposerSpanKind
import uniffi.teamcraft_ffi.composerActiveFormats
import uniffi.teamcraft_ffi.composerApplyFormat
import uniffi.teamcraft_ffi.composerBackspace
import uniffi.teamcraft_ffi.composerEnter

/**
 * The composer's formatting (#1051). The field holds the Markdown that is sent, so a format is an
 * edit of that text, made by the core (teamcraft-client-api `composer_format`, the same on iOS); the
 * styling shown while typing is presentation only ([ComposerStyleTransformation]).
 */

/** The colors of the live styling. */
data class ComposerStyleColors(
  /** Syntax characters (`**`, `> `, a fence). */
  val marker: Color,
  val quote: Color,
  val link: Color,
  val mention: Color,
  val codeBackground: Color,
)

/** What the core's edit of [format] (and the keys below) read and write: text and selection. */
fun TextFieldValue.formatted(format: ComposerFormat): TextFieldValue =
  composerApplyFormat(text, selection.min.toUInt(), selection.max.toUInt(), format).toValue()

/** The formats the selection has now, for the buttons' pressed state. */
fun TextFieldValue.activeFormats(): Set<ComposerFormat> =
  composerActiveFormats(text, selection.min.toUInt(), selection.max.toUInt()).toSet()

fun ComposerEdit.toValue(): TextFieldValue =
  TextFieldValue(text, TextRange(selectionStart.toInt(), selectionEnd.toInt()))

/**
 * The text with the core's [spans] as styles. The text is the same characters, so the caret, the
 * selection and the input method's composition are where they were.
 */
fun styledComposerText(text: String, spans: List<ComposerSpan>, colors: ComposerStyleColors): AnnotatedString {
  val builder = AnnotatedString.Builder(text)
  for (span in spans) {
    val start = span.start.toInt().coerceIn(0, text.length)
    val end = span.end.toInt().coerceIn(start, text.length)
    if (start < end) builder.addStyle(styleOf(span.kind, colors), start, end)
  }
  return builder.toAnnotatedString()
}

internal fun styleOf(kind: ComposerSpanKind, colors: ComposerStyleColors): SpanStyle = when (kind) {
  ComposerSpanKind.MARKER -> SpanStyle(color = colors.marker)
  ComposerSpanKind.BOLD -> SpanStyle(fontWeight = FontWeight.Bold)
  ComposerSpanKind.ITALIC -> SpanStyle(fontStyle = FontStyle.Italic)
  ComposerSpanKind.UNDERLINE -> SpanStyle(textDecoration = TextDecoration.Underline)
  ComposerSpanKind.STRIKE -> SpanStyle(textDecoration = TextDecoration.LineThrough)
  ComposerSpanKind.CODE -> SpanStyle(fontFamily = FontFamily.Monospace, background = colors.codeBackground)
  ComposerSpanKind.CODE_BLOCK -> SpanStyle(fontFamily = FontFamily.Monospace, background = colors.codeBackground)
  ComposerSpanKind.QUOTE -> SpanStyle(color = colors.quote)
  ComposerSpanKind.HEADING -> SpanStyle(fontWeight = FontWeight.Bold)
  ComposerSpanKind.LINK -> SpanStyle(color = colors.link, textDecoration = TextDecoration.Underline)
  ComposerSpanKind.MENTION -> SpanStyle(color = colors.mention, fontWeight = FontWeight.Bold)
}

/**
 * Shows [styled] in the field. It maps every offset to itself and adds only styles, so what is typed,
 * the caret and the composition are not touched; for a text other than the one styled (a keystroke
 * ahead of the styling) it shows the text as it is.
 */
class ComposerStyleTransformation(private val styled: AnnotatedString) : VisualTransformation {
  override fun filter(text: AnnotatedString): TransformedText {
    val shown = if (text.text == styled.text) styled else text
    return TransformedText(shown, OffsetMapping.Identity)
  }
}

/**
 * A change the keyboard made that is Enter or Backspace at a block, and what the core makes of it:
 * Enter goes on in a list or quote (an empty item leaves it), Backspace after a marker takes it off.
 * `null` leaves the change as it is. Never while the input method composes (before or after the
 * change): a composing text isn't ours to edit.
 */
fun interceptKey(
  old: TextFieldValue,
  new: TextFieldValue,
  enter: (String, Int) -> ComposerEdit? = { text, at -> composerEnter(text, at.toUInt(), at.toUInt()) },
  backspace: (String, Int) -> ComposerEdit? = { text, at -> composerBackspace(text, at.toUInt(), at.toUInt()) },
): TextFieldValue? {
  if (old.composition != null || new.composition != null || !old.selection.collapsed || !new.selection.collapsed) return null
  val at = old.selection.start
  if (at > old.text.length) return null
  if (new.text.length == old.text.length + 1 && new.selection.start == at + 1 &&
    new.text == old.text.substring(0, at) + "\n" + old.text.substring(at)
  ) {
    return enter(old.text, at)?.toValue()
  }
  if (at > 0 && new.text.length == old.text.length - 1 && new.selection.start == at - 1 &&
    new.text == old.text.removeRange(at - 1, at)
  ) {
    return backspace(old.text, at)?.toValue()
  }
  return null
}
