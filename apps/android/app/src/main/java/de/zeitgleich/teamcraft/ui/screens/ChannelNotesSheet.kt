package de.zeitgleich.teamcraft.ui.screens

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.IntrinsicSize
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Checkbox
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Text
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.LinkAnnotation
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.TextLinkStyles
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.text.withLink
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.ChatActions
import de.zeitgleich.teamcraft.core.ChatQueries
import de.zeitgleich.teamcraft.core.FileInfo
import de.zeitgleich.teamcraft.ui.MainViewModel
import de.zeitgleich.teamcraft.ui.ServerHistory
import de.zeitgleich.teamcraft.ui.theme.Spacing
import uniffi.teamcraft_ffi.CanvasBlockItem
import uniffi.teamcraft_ffi.CanvasBlockKind
import uniffi.teamcraft_ffi.CanvasPageItem
import uniffi.teamcraft_ffi.CanvasRunItem

/** Pages of older history read per opening, as on desktop and web (e2ee.md §4.11, "A device catching up"). */
private const val MAX_HISTORY_PAGES = 20

/** How light the code block's background is over the sheet's surface. */
private const val CODE_BACKGROUND_ALPHA = 0.06f

/**
 * A channel's shared notes, read-only (#1456, e2ee.md §4.11): the page as desktop and web show it,
 * built by the core from the updates this device holds. Editing stays on desktop and web for now;
 * the note under the title says so. While the sheet is open the channel's older history is read a
 * page at a time, since the page's edits are messages in it.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ChannelNotesSheet(viewModel: MainViewModel, conversationId: String, onDismiss: () -> Unit) {
  val changes by viewModel.changeCount.collectAsState()
  var maybePageId by rememberSaveable(conversationId) { mutableStateOf<String?>(null) }
  var maybePage by remember(conversationId) { mutableStateOf<CanvasPageItem?>(null) }
  LaunchedEffect(conversationId, maybePageId, changes) {
    maybePage = viewModel.canvasPage(conversationId, maybePageId) ?: maybePage
  }
  var serverHistory by remember(conversationId) { mutableStateOf(ServerHistory.COMPLETE) }
  var historyPages by remember(conversationId) { mutableIntStateOf(0) }
  LaunchedEffect(conversationId, changes) {
    serverHistory = viewModel.readChat { query(ChatQueries.olderHistory(conversationId)).olderHistory } ?: serverHistory
    if (serverHistory == ServerHistory.AVAILABLE && historyPages < MAX_HISTORY_PAGES) {
      historyPages += 1
      viewModel.act(ChatActions.loadOlderHistory(conversationId))
    }
  }
  val hasFiles = maybePage?.blocks?.any { it.maybeFile != null } == true
  val files = rememberRead(viewModel, key = conversationId, enabled = hasFiles) { query(ChatQueries.files(conversationId)).files }
  val context = LocalContext.current
  ModalBottomSheet(onDismissRequest = onDismiss, sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)) {
    val page = maybePage
    LazyColumn(
      Modifier.fillMaxWidth().padding(horizontal = Spacing.l).padding(bottom = Spacing.xxl),
      verticalArrangement = Arrangement.spacedBy(Spacing.s),
    ) {
      item {
        Text(
          stringResource(R.string.channel_notes_title),
          style = MaterialTheme.typography.titleMedium,
          modifier = Modifier.semantics { heading() },
        )
      }
      if (page == null) {
        item { PageLoadingIndicator(Modifier.padding(Spacing.l)) }
        return@LazyColumn
      }
      if (page.pages.isNotEmpty()) {
        item { PageChips(page, onPick = { maybePageId = it }) }
      }
      item { NotesHint(stringResource(R.string.channel_notes_read_only_phone)) }
      notesStatus(page, serverHistory)?.let { status -> item { NotesHint(stringResource(status), isStatus = true) } }
      if (page.isUnreadable) return@LazyColumn
      if (page.blocks.isEmpty()) {
        item { EmptyNotes() }
        return@LazyColumn
      }
      items(page.blocks, key = { it.id }) { block ->
        NotesBlock(block, files.maybeValue.orEmpty(), onOpenFile = { file -> openOrDownload(viewModel, file, context) })
      }
    }
  }
}

/** The page's line of state: unreadable, loading or missing earlier changes; `null` when all is here. */
private fun notesStatus(page: CanvasPageItem, serverHistory: String): Int? = when {
  page.isUnreadable -> R.string.channel_notes_unreadable
  serverHistory == ServerHistory.LOADING -> R.string.channel_notes_loading_history
  serverHistory == ServerHistory.FAILED -> R.string.channel_notes_history_failed
  page.isIncomplete -> R.string.channel_notes_incomplete
  else -> null
}

/** The first page and the further ones the first page lists, as chips; the shown one selected. */
@Composable
private fun PageChips(page: CanvasPageItem, onPick: (String?) -> Unit) {
  val label = stringResource(R.string.channel_notes_pages)
  Row(
    Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()).semantics { contentDescription = label },
    horizontalArrangement = Arrangement.spacedBy(Spacing.s),
  ) {
    FilterChip(
      selected = page.maybePageId == null,
      onClick = { onPick(null) },
      label = { Text(stringResource(R.string.channel_notes_title)) },
    )
    for (entry in page.pages) {
      FilterChip(
        selected = page.maybePageId == entry.pageId,
        onClick = { onPick(entry.pageId) },
        label = { Text(entry.title.ifBlank { stringResource(R.string.channel_notes_page_untitled) }, maxLines = 1) },
        modifier = Modifier.widthIn(max = 200.dp),
      )
    }
  }
}

@Composable
private fun EmptyNotes() {
  Column(Modifier.fillMaxWidth().padding(vertical = Spacing.xl), horizontalAlignment = Alignment.CenterHorizontally) {
    Text(stringResource(R.string.channel_notes_empty_title), style = MaterialTheme.typography.titleSmall)
    Text(
      stringResource(R.string.channel_notes_empty),
      style = MaterialTheme.typography.bodyMedium,
      color = MaterialTheme.colorScheme.onSurfaceVariant,
      modifier = Modifier.padding(top = Spacing.xs),
    )
  }
}

/** One block: its marker (bullet, number, check box, quote bar) and its formatted text. */
@Composable
private fun NotesBlock(block: CanvasBlockItem, files: List<FileInfo>, onOpenFile: (FileInfo) -> Unit) {
  val scheme = MaterialTheme.colorScheme
  val typography = MaterialTheme.typography
  when (block.kind) {
    CanvasBlockKind.HEADING -> BlockText(block, typography.titleLarge, Modifier.padding(top = Spacing.s).semantics { heading() })
    CanvasBlockKind.HEADING2 -> BlockText(block, typography.titleMedium, Modifier.padding(top = Spacing.xs).semantics { heading() })
    CanvasBlockKind.HEADING3 -> BlockText(block, typography.titleSmall, Modifier.semantics { heading() })
    CanvasBlockKind.BULLET -> MarkedRow("•") { BlockText(block, typography.bodyLarge) }
    CanvasBlockKind.NUMBERED -> MarkedRow("${block.number}.") { BlockText(block, typography.bodyLarge) }
    CanvasBlockKind.CHECK -> CheckRow(block)
    CanvasBlockKind.QUOTE -> Row(Modifier.height(IntrinsicSize.Min)) {
      Box(Modifier.width(3.dp).fillMaxHeight().background(scheme.outlineVariant, RoundedCornerShape(2.dp)))
      BlockText(block, typography.bodyLarge.copy(color = scheme.onSurfaceVariant), Modifier.padding(start = Spacing.m))
    }
    CanvasBlockKind.CODE -> Box(
      Modifier.fillMaxWidth()
        .background(scheme.onSurface.copy(alpha = CODE_BACKGROUND_ALPHA), RoundedCornerShape(8.dp))
        .horizontalScroll(rememberScrollState())
        .padding(Spacing.m),
    ) {
      Text(block.runs.joinToString("") { it.text }, style = typography.bodyMedium.copy(fontFamily = FontFamily.Monospace), softWrap = false)
    }
    CanvasBlockKind.FILE -> FileRow(block, files, onOpenFile)
    CanvasBlockKind.PARAGRAPH -> BlockText(block, typography.bodyLarge)
  }
}

@Composable
private fun MarkedRow(marker: String, content: @Composable () -> Unit) {
  Row {
    Text(
      marker,
      style = MaterialTheme.typography.bodyLarge,
      color = MaterialTheme.colorScheme.onSurfaceVariant,
      modifier = Modifier.widthIn(min = 20.dp).padding(end = Spacing.s),
    )
    content()
  }
}

/** A checklist item: the box shows the mark (read-only here), a done item's text is muted. */
@Composable
private fun CheckRow(block: CanvasBlockItem) {
  val scheme = MaterialTheme.colorScheme
  val state = stringResource(if (block.isChecked) R.string.channel_notes_checked else R.string.channel_notes_unchecked)
  Row(verticalAlignment = Alignment.Top) {
    // Read-only here: no change handler, so it shows the mark and takes no taps.
    Checkbox(
      checked = block.isChecked,
      onCheckedChange = null,
      modifier = Modifier.padding(end = Spacing.s).size(20.dp).semantics { contentDescription = state },
    )
    val style = MaterialTheme.typography.bodyLarge
    BlockText(block, if (block.isChecked) style.copy(color = scheme.onSurfaceVariant) else style)
  }
}

/**
 * A file of the channel in the page, shown from the message it was sent with: opened like from the
 * channel's files, or named as missing when this device holds no such message.
 */
@Composable
private fun FileRow(block: CanvasBlockItem, files: List<FileInfo>, onOpenFile: (FileInfo) -> Unit) {
  val scheme = MaterialTheme.colorScheme
  val maybeFile = block.maybeFile?.let { ref -> files.firstOrNull { it.fileId == ref.fileId && it.message == ref.messageId } }
  val name = maybeFile?.fileName ?: block.runs.joinToString("") { it.text }
  val detail = when {
    maybeFile == null -> stringResource(R.string.channel_notes_file_missing)
    maybeFile.transfer.isMoving -> stringResource(R.string.channel_notes_file_downloading)
    maybeFile.transfer.state == "local" -> stringResource(R.string.channel_notes_file_open)
    else -> stringResource(R.string.channel_notes_file_download)
  }
  val rowModifier = Modifier.fillMaxWidth()
    .background(scheme.surfaceContainerHigh, RoundedCornerShape(12.dp))
  Row(
    (if (maybeFile != null) rowModifier.clickable(onClickLabel = stringResource(R.string.file_open)) { onOpenFile(maybeFile) } else rowModifier).padding(Spacing.m),
    verticalAlignment = Alignment.CenterVertically,
  ) {
    Icon(painterResource(R.drawable.ic_description), contentDescription = null, tint = scheme.onSurfaceVariant)
    Column(Modifier.padding(start = Spacing.m)) {
      Text(name, style = MaterialTheme.typography.bodyLarge, maxLines = 2)
      Text(detail, style = MaterialTheme.typography.bodySmall, color = scheme.onSurfaceVariant)
    }
  }
}

/** A block's text with its formatting; web links open in the browser. */
@Composable
private fun BlockText(block: CanvasBlockItem, style: TextStyle, modifier: Modifier = Modifier) {
  val context = LocalContext.current
  val linkColor = MaterialTheme.colorScheme.primary
  val codeBackground = MaterialTheme.colorScheme.onSurface.copy(alpha = CODE_BACKGROUND_ALPHA)
  val text = remember(block.runs, linkColor, codeBackground) {
    annotated(block.runs, linkColor, codeBackground) { url -> openLink(context, url) }
  }
  Text(text, style = style, modifier = modifier)
}

private fun annotated(runs: List<CanvasRunItem>, linkColor: Color, codeBackground: Color, onOpen: (String) -> Unit): AnnotatedString =
  buildAnnotatedString {
    for (run in runs) {
      val span = SpanStyle(
        fontWeight = if (run.isBold) FontWeight.SemiBold else null,
        fontStyle = if (run.isItalic) FontStyle.Italic else null,
        textDecoration = if (run.isStrike) TextDecoration.LineThrough else null,
        fontFamily = if (run.isCode) FontFamily.Monospace else null,
        background = if (run.isCode) codeBackground else Color.Unspecified,
      )
      val maybeLink = run.maybeLink
      if (maybeLink != null) {
        val styles = TextLinkStyles(span.merge(SpanStyle(color = linkColor, textDecoration = TextDecoration.Underline)))
        withLink(LinkAnnotation.Clickable(tag = maybeLink, styles = styles) { onOpen(maybeLink) }) { append(run.text) }
      } else {
        withStyle(span) { append(run.text) }
      }
    }
  }

@Composable
private fun NotesHint(text: String, isStatus: Boolean = false) {
  Text(
    text,
    style = MaterialTheme.typography.bodySmall,
    color = MaterialTheme.colorScheme.onSurfaceVariant,
    modifier = if (isStatus) Modifier.semantics { liveRegion = LiveRegionMode.Polite } else Modifier,
  )
}
