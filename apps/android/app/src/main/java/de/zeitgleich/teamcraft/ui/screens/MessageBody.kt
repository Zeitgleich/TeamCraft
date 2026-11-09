package de.zeitgleich.teamcraft.ui.screens

import androidx.compose.foundation.background
import androidx.compose.foundation.border
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
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.layout.Layout
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.Constraints
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.ui.LinkHandlers
import de.zeitgleich.teamcraft.ui.MarkupColors
import de.zeitgleich.teamcraft.ui.MentionNames
import de.zeitgleich.teamcraft.ui.annotatedLine
import de.zeitgleich.teamcraft.ui.annotatedLines
import uniffi.teamcraft_ffi.MarkupAlignment
import uniffi.teamcraft_ffi.MarkupBlock
import uniffi.teamcraft_ffi.MarkupLine
import uniffi.teamcraft_ffi.MarkupListKind
import uniffi.teamcraft_ffi.isOpenableLink
import uniffi.teamcraft_ffi.jumboEmojiScale
import uniffi.teamcraft_ffi.messageMarkup

/**
 * A message body in the shared Markdown subset, rendered like desktop and web (issue #704):
 * paragraphs, headings, quotes, lists, code blocks and tables; the inline styles come from
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
  // Only 1-3 emoji: drawn larger (jumbomoji, #1603); the core decides, as on web, desktop and iOS.
  // sp keeps following the user's font scale.
  val jumbo = remember(body) { jumboEmojiScale(body) }
  val paragraphStyle = if (jumbo > 1f) {
    bodyStyle.copy(fontSize = bodyStyle.fontSize * jumbo, lineHeight = bodyStyle.fontSize * jumbo * JUMBO_LINE_HEIGHT)
  } else {
    bodyStyle
  }
  Column(modifier, verticalArrangement = Arrangement.spacedBy(BLOCK_GAP)) {
    for (block in blocks) {
      when (block) {
        is MarkupBlock.Paragraph -> Text(
          remember(block, colors, names) { annotatedLines(block.lines, colors, names, links, permalinkLabel) },
          style = paragraphStyle,
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
        is MarkupBlock.Table -> MarkupTable(block, textColor, bodyStyle) { line ->
          remember(line, colors, names) { annotatedLine(line, colors, names, links, permalinkLabel) }
        }
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

/**
 * A table on hairlines, the header in semibold on a light fill. Columns are as wide as their widest
 * cell up to [MAX_CELL_WIDTH] (longer text wraps); wider than the bubble, the table scrolls sideways.
 */
@Composable
private fun MarkupTable(
  table: MarkupBlock.Table,
  textColor: Color,
  bodyStyle: TextStyle,
  styled: @Composable (MarkupLine) -> AnnotatedString,
) {
  val columns = table.header.size
  val rows = listOf(table.header) + table.rows.map { it.cells }
  val line = textColor.copy(alpha = TABLE_LINE_ALPHA)
  val headerFill = textColor.copy(alpha = TABLE_HEADER_ALPHA)
  val cellStyle = MaterialTheme.typography.bodyMedium.copy(
    color = textColor,
    letterSpacing = bodyStyle.letterSpacing,
    fontFeatureSettings = "tnum",
  )
  val shape = RoundedCornerShape(CODE_CORNER)
  Box(Modifier.border(1.dp, line, shape).clip(shape).horizontalScroll(rememberScrollState())) {
    Layout(
      content = {
        rows.forEachIndexed { rowIndex, cells ->
          val isHeader = rowIndex == 0
          for (column in 0 until columns) {
            val cell = cells.getOrNull(column) ?: MarkupLine(emptyList())
            Text(
              styled(cell),
              style = if (isHeader) cellStyle.copy(fontWeight = FontWeight.SemiBold) else cellStyle,
              textAlign = textAlign(table.alignments.getOrNull(column)),
              modifier = Modifier
                .drawBehind {
                  if (isHeader) drawRect(headerFill)
                  if (rowIndex < rows.lastIndex) {
                    drawLine(line, Offset(0f, size.height - 0.5f), Offset(size.width, size.height - 0.5f), 1.dp.toPx())
                  }
                  if (column > 0) drawLine(line, Offset(0.5f, 0f), Offset(0.5f, size.height), 1.dp.toPx())
                }
                .padding(horizontal = 10.dp, vertical = 5.dp),
            )
          }
        }
      },
    ) { measurables, _ ->
      // Column widths and row heights from the intrinsics, then every cell measured once at its
      // column's width and its row's height, so backgrounds and lines fill the grid.
      val maxCell = MAX_CELL_WIDTH.roundToPx()
      val widths = IntArray(columns)
      measurables.forEachIndexed { index, cell ->
        val column = index % columns
        widths[column] = maxOf(widths[column], minOf(cell.maxIntrinsicWidth(Constraints.Infinity), maxCell))
      }
      val rowCount = if (columns == 0) 0 else measurables.size / columns
      val heights = IntArray(rowCount) { row ->
        (0 until columns).maxOf { column -> measurables[row * columns + column].minIntrinsicHeight(widths[column]) }
      }
      val placeables = measurables.mapIndexed { index, cell ->
        cell.measure(Constraints.fixed(widths[index % columns], heights[index / columns]))
      }
      layout(widths.sum(), heights.sum()) {
        var y = 0
        for (row in 0 until rowCount) {
          var x = 0
          for (column in 0 until columns) {
            placeables[row * columns + column].place(x, y)
            x += widths[column]
          }
          y += heights[row]
        }
      }
    }
  }
}

private fun textAlign(alignment: MarkupAlignment?): TextAlign = when (alignment) {
  MarkupAlignment.LEFT -> TextAlign.Left
  MarkupAlignment.CENTER -> TextAlign.Center
  MarkupAlignment.RIGHT -> TextAlign.Right
  MarkupAlignment.NATURAL, null -> TextAlign.Start
}

@Composable
private fun headingStyle(level: Int): TextStyle = when (level) {
  1 -> MaterialTheme.typography.titleLarge
  2 -> MaterialTheme.typography.titleMedium
  else -> MaterialTheme.typography.titleSmall.copy(fontWeight = FontWeight.SemiBold)
}

private val BLOCK_GAP = 6.dp
private val BODY_TRACKING = 0.15.sp

/** Line height of jumbomoji as a multiple of their font size. */
private const val JUMBO_LINE_HEIGHT = 1.2f
private val QUOTE_BAR = 3.dp
private val CODE_CORNER = 8.dp
private val MAX_CELL_WIDTH = 240.dp
private const val TABLE_LINE_ALPHA = 0.2f
private const val TABLE_HEADER_ALPHA = 0.06f
private const val QUOTE_BAR_ALPHA = 0.45f
private const val QUOTE_TEXT_ALPHA = 0.85f
