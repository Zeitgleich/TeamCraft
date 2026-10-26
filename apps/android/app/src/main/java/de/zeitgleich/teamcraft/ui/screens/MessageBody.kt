package de.zeitgleich.teamcraft.ui.screens

import androidx.compose.foundation.background
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.IntrinsicSize
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.ui.LinkHandlers
import de.zeitgleich.teamcraft.ui.MarkupColors
import de.zeitgleich.teamcraft.ui.MentionNames
import de.zeitgleich.teamcraft.ui.annotatedLine
import de.zeitgleich.teamcraft.ui.annotatedLines
import uniffi.teamcraft_ffi.MarkupBlock
import uniffi.teamcraft_ffi.MarkupListKind
import uniffi.teamcraft_ffi.isOpenableLink
import uniffi.teamcraft_ffi.messageMarkup

/**
 * A message body in the shared Markdown subset, rendered like desktop and web (issue #704):
 * paragraphs, headings, quotes, lists and code blocks; the inline styles come from
 * [annotatedLine]. The core parses, so every platform reads a body the same way.
 */
@Composable
fun MessageBody(
  body: String,
  textColor: Color,
  colors: MarkupColors,
  names: MentionNames,
  onOpenPermalink: (String) -> Unit,
  modifier: Modifier = Modifier,
) {
  val context = LocalContext.current
  val blocks = remember(body) { messageMarkup(body) }
  val links = remember(context, onOpenPermalink) {
    LinkHandlers(isOpenable = ::isOpenableLink, onOpenWeb = { openLink(context, it) }, onOpenPermalink = onOpenPermalink)
  }
  val permalinkLabel = stringResource(R.string.message_link_label)
  // bodyLarge with chat-style tracking: the scale's 0.5 sp reads airy in a bubble.
  val bodyStyle = MaterialTheme.typography.bodyLarge.copy(color = textColor, letterSpacing = BODY_TRACKING)
  Column(modifier, verticalArrangement = Arrangement.spacedBy(BLOCK_GAP)) {
    for (block in blocks) {
      when (block) {
        is MarkupBlock.Paragraph -> Text(
          remember(block, colors, names) { annotatedLines(block.lines, colors, names, links, permalinkLabel) },
          style = bodyStyle,
        )
        is MarkupBlock.Heading -> Text(
          remember(block, colors, names) { annotatedLine(block.line, colors, names, links, permalinkLabel) },
          style = headingStyle(block.level.toInt()).copy(color = textColor),
        )
        is MarkupBlock.Quote -> Row(Modifier.height(IntrinsicSize.Min)) {
          Box(Modifier.width(QUOTE_BAR).fillMaxHeight().background(textColor.copy(alpha = QUOTE_BAR_ALPHA), RoundedCornerShape(2.dp)))
          Text(
            remember(block, colors, names) { annotatedLines(block.lines, colors, names, links, permalinkLabel) },
            style = bodyStyle.copy(color = textColor.copy(alpha = QUOTE_TEXT_ALPHA)),
            modifier = Modifier.padding(start = 8.dp),
          )
        }
        is MarkupBlock.Items -> Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
          block.items.forEachIndexed { index, item ->
            val marker = when (val kind = block.kind) {
              MarkupListKind.Bulleted -> "•"
              is MarkupListKind.Numbered -> "${kind.start.toLong() + index}."
            }
            Row {
              Text(marker, style = bodyStyle, modifier = Modifier.padding(end = 6.dp))
              Text(remember(item, colors, names) { annotatedLine(item, colors, names, links, permalinkLabel) }, style = bodyStyle)
            }
          }
        }
        is MarkupBlock.Code -> CodeBlock(block.code, textColor, colors.codeBackground)
      }
    }
  }
}

/** Fenced code: monospace on its own surface, scrolled sideways rather than wrapped. */
@Composable
private fun CodeBlock(code: String, textColor: Color, background: Color) {
  Box(
    Modifier
      .background(background, RoundedCornerShape(CODE_CORNER))
      .horizontalScroll(rememberScrollState())
      .padding(horizontal = 10.dp, vertical = 8.dp),
  ) {
    Text(
      code,
      style = MaterialTheme.typography.bodyMedium.copy(fontFamily = FontFamily.Monospace, color = textColor),
      softWrap = false,
    )
  }
}

@Composable
private fun headingStyle(level: Int): TextStyle = when (level) {
  1 -> MaterialTheme.typography.titleLarge
  2 -> MaterialTheme.typography.titleMedium
  else -> MaterialTheme.typography.titleSmall.copy(fontWeight = FontWeight.SemiBold)
}

private val BLOCK_GAP = 6.dp
private val BODY_TRACKING = 0.15.sp
private val QUOTE_BAR = 3.dp
private val CODE_CORNER = 8.dp
private const val QUOTE_BAR_ALPHA = 0.45f
private const val QUOTE_TEXT_ALPHA = 0.85f
