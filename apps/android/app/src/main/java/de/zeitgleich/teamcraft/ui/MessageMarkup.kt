package de.zeitgleich.teamcraft.ui

import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.LinkAnnotation
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.TextLinkStyles
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.text.withLink
import androidx.compose.ui.text.withStyle
import uniffi.teamcraft_ffi.MarkupLine
import uniffi.teamcraft_ffi.MarkupRun
import uniffi.teamcraft_ffi.MarkupRunKind

/** How message permalinks start (the core's format, teamcraft-client-api `markdown`). */
const val PERMALINK_PREFIX = "teamcraft:/c/"

/** A bare link longer than this shows shortened (`example.com/…/page`); the whole URL stays its target. */
const val MAX_LINK_LABEL_CHARS = 40

/** The colors of a body's runs, which differ inside an own (accent) bubble and another's. */
data class MarkupColors(
  val link: Color,
  val mention: Color,
  val mentionMe: Color,
  val mentionMeBackground: Color,
  val codeBackground: Color,
)

/** Who can be mentioned, to tell `@alice` from an e-mail-like word. */
data class MentionNames(val usernames: Set<String>, val myUsername: String)

/** What a tapped link does. */
class LinkHandlers(
  /** The shared rule (teamcraft-client-api `links`): only these open in the browser. */
  val isOpenable: (String) -> Boolean,
  val onOpenWeb: (String) -> Unit,
  val onOpenPermalink: (String) -> Unit,
)

/**
 * One line of the core's markup (the Markdown subset desktop and web render) as styled text: bold,
 * italic, underline and strike as spans, inline code in a monospace box without its backticks,
 * links tappable with long URLs shortened, and mentions of known people highlighted.
 * [permalinkLabel] stands in for a link to a message.
 */
fun annotatedLine(
  line: MarkupLine,
  colors: MarkupColors,
  names: MentionNames,
  links: LinkHandlers,
  permalinkLabel: String,
): AnnotatedString = buildAnnotatedString { appendLine(line, colors, names, links, permalinkLabel) }

/** Several lines (a paragraph or quote) with a line break between them. */
fun annotatedLines(
  lines: List<MarkupLine>,
  colors: MarkupColors,
  names: MentionNames,
  links: LinkHandlers,
  permalinkLabel: String,
): AnnotatedString = buildAnnotatedString {
  lines.forEachIndexed { index, line ->
    if (index > 0) append('\n')
    appendLine(line, colors, names, links, permalinkLabel)
  }
}

/**
 * A bare link's label: without `https://` and `www.`, and when still long, the host plus the end
 * of the path (`example.com/…/report.pdf`), cut with an ellipsis at [maxChars].
 */
fun shortLinkLabel(url: String, maxChars: Int = MAX_LINK_LABEL_CHARS): String {
  val bare = url.substringAfter("://").removePrefix("www.").trimEnd('/')
  if (bare.length <= maxChars) return bare
  val host = bare.substringBefore('/')
  val tail = bare.substringAfterLast('/', "").substringBefore('?').substringBefore('#')
  val shortened = if (tail.isNotEmpty() && host != bare) "$host/…/$tail" else bare
  return if (shortened.length <= maxChars) shortened else shortened.take(maxChars - 1) + "…"
}

private fun AnnotatedString.Builder.appendLine(
  line: MarkupLine,
  colors: MarkupColors,
  names: MentionNames,
  links: LinkHandlers,
  permalinkLabel: String,
) {
  for (run in line.runs) {
    val isPlain = !run.isBold && !run.isItalic && !run.isUnderline && !run.isStrike
    if (isPlain) {
      appendRun(run, colors, names, links, permalinkLabel)
    } else {
      withStyle(emphasis(run)) { appendRun(run, colors, names, links, permalinkLabel) }
    }
  }
}

private fun AnnotatedString.Builder.appendRun(
  run: MarkupRun,
  colors: MarkupColors,
  names: MentionNames,
  links: LinkHandlers,
  permalinkLabel: String,
) {
  when (val kind = run.kind) {
    MarkupRunKind.Text -> append(run.text)
    // A thin space each side gives the code box room without changing what is copied much.
    MarkupRunKind.Code -> withStyle(SpanStyle(fontFamily = FontFamily.Monospace, background = colors.codeBackground)) {
      append(" ${run.text} ")
    }
    is MarkupRunKind.Link -> appendLink(kind, run.text, colors, links, permalinkLabel)
    is MarkupRunKind.Mention -> {
      val name = kind.name.lowercase()
      val isMe = name == names.myUsername.lowercase() || name in CHANNEL_MENTIONS
      when {
        isMe -> withStyle(SpanStyle(color = colors.mentionMe, background = colors.mentionMeBackground, fontWeight = FontWeight.Bold)) {
          append(run.text)
        }
        name in names.usernames -> withStyle(SpanStyle(color = colors.mention, fontWeight = FontWeight.Bold)) { append(run.text) }
        // Not a member's name (an address, a handle elsewhere): plain text.
        else -> append(run.text)
      }
    }
  }
}

private fun AnnotatedString.Builder.appendLink(
  link: MarkupRunKind.Link,
  text: String,
  colors: MarkupColors,
  links: LinkHandlers,
  permalinkLabel: String,
) {
  val styles = TextLinkStyles(SpanStyle(color = colors.link, textDecoration = TextDecoration.Underline))
  val url = link.url
  when {
    url.startsWith(PERMALINK_PREFIX) -> {
      val label = if (link.isLabeled) text else permalinkLabel
      withLink(LinkAnnotation.Clickable(tag = url, styles = styles) { links.onOpenPermalink(url) }) { append(label) }
    }
    links.isOpenable(url) -> {
      val label = if (link.isLabeled) text else shortLinkLabel(url)
      withLink(LinkAnnotation.Clickable(tag = url, styles = styles) { links.onOpenWeb(url) }) { append(label) }
    }
    // Not a link the shared rule opens: the text as written.
    else -> append(text)
  }
}

private fun emphasis(run: MarkupRun): SpanStyle {
  val decorations = listOfNotNull(
    TextDecoration.Underline.takeIf { run.isUnderline },
    TextDecoration.LineThrough.takeIf { run.isStrike },
  )
  return SpanStyle(
    fontWeight = if (run.isBold) FontWeight.Bold else null,
    fontStyle = if (run.isItalic) FontStyle.Italic else null,
    textDecoration = if (decorations.isEmpty()) null else TextDecoration.combine(decorations),
  )
}
