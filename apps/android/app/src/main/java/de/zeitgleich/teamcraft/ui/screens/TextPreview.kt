package de.zeitgleich.teamcraft.ui.screens

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.SharedPreferences
import android.widget.Toast
import androidx.annotation.StringRes
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.IntrinsicSize
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.KeyboardArrowDown
import androidx.compose.material.icons.filled.KeyboardArrowUp
import androidx.compose.material.icons.filled.Search
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.ui.SharedFiles
import de.zeitgleich.teamcraft.ui.TEXT_PREVIEW_MAX_BYTES
import de.zeitgleich.teamcraft.ui.TextPreviewRequest
import de.zeitgleich.teamcraft.ui.textPreviewTypeKey
import kotlinx.coroutines.delay
import uniffi.teamcraft_ffi.CellHit
import uniffi.teamcraft_ffi.ColumnInfo
import uniffi.teamcraft_ffi.DelimiterChoice
import uniffi.teamcraft_ffi.LineMatch
import uniffi.teamcraft_ffi.PreviewEncoding
import uniffi.teamcraft_ffi.TableInfo
import uniffi.teamcraft_ffi.TextPreview
import uniffi.teamcraft_ffi.TextPreviewException
import uniffi.teamcraft_ffi.openTextPreview

private const val PREFS = "text_preview"
private const val FIND_DEBOUNCE_MS = 250L
private const val MAX_SEPARATOR_BYTES = 8
/** A column's width per character of its widest cell, and the room around it. */
private val CHAR_WIDTH = 8.dp
private val CELL_PADDING = 10.dp
private val GUTTER_DIGIT = 9.dp

/** Draws the text preview the app asks for (#2342); once, at the root of each activity. */
@Composable
fun TextPreviewHost() {
  TextPreviewRequest.current?.let { target -> TextPreviewScreen(target, onDismiss = TextPreviewRequest::dismiss) }
}

/** What opening the file came to. */
private sealed interface Opened {
  data object Loading : Opened
  data class Failed(@StringRes val message: Int) : Opened
  data class Ready(val preview: TextPreview) : Opened
}

/** The table's state next to the text: not a table, parsing, shown, or refused with the reason. */
private sealed interface TableState {
  data object None : TableState
  data object Parsing : TableState
  data class Ready(val info: TableInfo) : TableState
  data class Refused(@StringRes val message: Int, val arg: Int? = null) : TableState
}

/** A file's text or table in a full-screen dialog: find, separator, header, wrapping, copy. */
@Composable
private fun TextPreviewScreen(target: TextPreviewRequest.Target, onDismiss: () -> Unit) {
  val context = LocalContext.current
  var opened by remember(target) { mutableStateOf<Opened>(Opened.Loading) }
  // Parsing and the file read run off the UI thread inside the core (#2184).
  LaunchedEffect(target) {
    opened = try {
      Opened.Ready(openTextPreview(target.file.path, TEXT_PREVIEW_MAX_BYTES.toULong()))
    } catch (failure: TextPreviewException.NotText) {
      Opened.Failed(R.string.text_preview_not_text)
    } catch (failure: TextPreviewException) {
      Opened.Failed(R.string.text_preview_unreadable)
    }
  }
  val latest by rememberUpdatedState(opened)
  DisposableEffect(target) { onDispose { (latest as? Opened.Ready)?.preview?.close() } }

  Dialog(onDismissRequest = onDismiss, properties = DialogProperties(usePlatformDefaultWidth = false)) {
    Surface(Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.background) {
      Column(Modifier.fillMaxSize().safeDrawingPadding()) {
        // A file that is read draws its own bar, with find.
        if (opened !is Opened.Ready) PreviewTopBar(target, onDismiss, onFind = null)
        when (val current = opened) {
          Opened.Loading -> Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) { PageLoadingIndicator() }
          is Opened.Failed -> Column(
            Modifier.fillMaxSize().padding(24.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp, Alignment.CenterVertically),
            horizontalAlignment = Alignment.CenterHorizontally,
          ) {
            Text(stringResource(current.message), textAlign = TextAlign.Center)
            TextButton(onClick = { SharedFiles.open(context, target.file, target.mediaType) }) {
              Text(stringResource(R.string.text_preview_open_elsewhere))
            }
          }
          is Opened.Ready -> ReadyPreview(current.preview, target, onDismiss)
        }
      }
    }
  }
}

@Composable
private fun PreviewTopBar(target: TextPreviewRequest.Target, onDismiss: () -> Unit, onFind: (() -> Unit)?) {
  val context = LocalContext.current
  Row(Modifier.fillMaxWidth().padding(horizontal = 4.dp), verticalAlignment = Alignment.CenterVertically) {
    IconButton(onClick = onDismiss) { Icon(Icons.Filled.Close, contentDescription = stringResource(R.string.close)) }
    Text(
      target.fileName,
      Modifier.weight(1f).padding(horizontal = 4.dp),
      style = MaterialTheme.typography.titleMedium,
      maxLines = 1,
      overflow = TextOverflow.Ellipsis,
    )
    if (onFind != null) {
      IconButton(onClick = onFind) { Icon(Icons.Filled.Search, contentDescription = stringResource(R.string.text_preview_find)) }
    }
    TextButton(onClick = { SharedFiles.open(context, target.file, target.mediaType) }) {
      Text(stringResource(R.string.text_preview_open_elsewhere), maxLines = 1)
    }
  }
}

@Composable
private fun ReadyPreview(preview: TextPreview, target: TextPreviewRequest.Target, onDismiss: () -> Unit) {
  val context = LocalContext.current
  val info = remember(preview) { preview.info() }
  val prefs = remember(context) { context.getSharedPreferences(PREFS, Context.MODE_PRIVATE) }
  val typeKey = remember(target) { textPreviewTypeKey(target.fileName, target.mediaType) }
  // Remembered per type of file, like on desktop and web.
  var separator by remember(typeKey) { mutableStateOf(Separator.read(prefs, typeKey)) }
  var maybeHeader by remember(typeKey) { mutableStateOf(HeaderPrefs.read(prefs, typeKey)) }
  var isWrapped by remember(typeKey) { mutableStateOf(prefs.getBoolean("$typeKey.wrap", false)) }
  var prefersTable by remember(typeKey) { mutableStateOf(prefs.getBoolean("$typeKey.table", true)) }

  var tableState by remember(preview) { mutableStateOf<TableState>(TableState.None) }
  LaunchedEffect(preview, separator) {
    val maybeChoice = separator.choice
    if (maybeChoice == null && info.maybeDetected == null) {
      tableState = TableState.None
      return@LaunchedEffect
    }
    tableState = TableState.Parsing
    tableState = try {
      TableState.Ready(preview.parseTable(maybeChoice))
    } catch (failure: TextPreviewException.UnterminatedQuote) {
      TableState.Refused(R.string.text_preview_unterminated_quote, failure.line.toInt())
    } catch (failure: TextPreviewException) {
      TableState.Refused(R.string.text_preview_not_a_table)
    }
  }
  val maybeTable = (tableState as? TableState.Ready)?.info
  val showsTable = maybeTable != null && prefersTable
  val hasHeader = maybeTable != null && (maybeHeader ?: maybeTable.looksLikeHeader)

  var isFinding by rememberSaveable(preview) { mutableStateOf(false) }
  var query by rememberSaveable(preview) { mutableStateOf("") }
  val lineMatches = remember { mutableStateOf<List<LineMatch>>(emptyList()) }
  val cellMatches = remember { mutableStateOf<List<CellHit>>(emptyList()) }
  var isCapped by remember { mutableStateOf(false) }
  var current by remember(query, showsTable, maybeTable) { mutableIntStateOf(0) }
  LaunchedEffect(query, showsTable, maybeTable) {
    if (query.isEmpty()) {
      lineMatches.value = emptyList()
      cellMatches.value = emptyList()
      isCapped = false
      return@LaunchedEffect
    }
    delay(FIND_DEBOUNCE_MS)
    if (showsTable) {
      val found = preview.findCells(query)
      cellMatches.value = found.matches
      isCapped = found.isCapped
    } else {
      val found = preview.findLines(query)
      lineMatches.value = found.matches
      isCapped = found.isCapped
    }
  }
  val matchCount = if (showsTable) cellMatches.value.size else lineMatches.value.size

  val listState = rememberLazyListState()
  LaunchedEffect(current, lineMatches.value, cellMatches.value, hasHeader) {
    if (showsTable) {
      cellMatches.value.getOrNull(current)?.let { hit ->
        val row = hit.row.toInt() - if (hasHeader) 1 else 0
        if (row >= 0) listState.animateScrollToItem(row)
      }
    } else {
      lineMatches.value.getOrNull(current)?.let { listState.animateScrollToItem(it.line.toInt()) }
    }
  }

  PreviewTopBar(target, onDismiss, onFind = { isFinding = !isFinding })
  if (isFinding) {
    FindBar(
      query = query,
      onQuery = { query = it },
      count = matchCount,
      current = current,
      isCapped = isCapped,
      onStep = { delta -> if (matchCount > 0) current = (current + delta + matchCount) % matchCount },
    )
  }
  Controls(
    canShowTable = maybeTable != null,
    showsTable = showsTable,
    onShowTable = { prefersTable = it; prefs.edit().putBoolean("$typeKey.table", it).apply() },
    separator = separator,
    detected = info.maybeDetected,
    onSeparator = { separator = it; Separator.write(prefs, typeKey, it) },
    hasHeader = hasHeader,
    onHeader = { maybeHeader = it; HeaderPrefs.write(prefs, typeKey, it) },
    isWrapped = isWrapped,
    onWrapped = { isWrapped = it; prefs.edit().putBoolean("$typeKey.wrap", it).apply() },
  )
  Notes(info.isTruncated, info.encoding, tableState, maybeTable)
  HorizontalDivider()
  when {
    maybeTable != null && prefersTable -> TableView(
      preview = preview,
      info = maybeTable,
      hasHeader = hasHeader,
      isWrapped = isWrapped,
      hits = cellMatches.value,
      current = cellMatches.value.getOrNull(current),
      listState = listState,
    )
    tableState == TableState.Parsing && prefersTable -> Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) { PageLoadingIndicator() }
    else -> TextView(
      preview = preview,
      lineCount = info.lineCount.toInt(),
      isWrapped = isWrapped,
      matches = lineMatches.value,
      current = lineMatches.value.getOrNull(current),
      listState = listState,
    )
  }
}

/** The view switch, the separator, the header switch and wrapping. */
@Composable
private fun Controls(
  canShowTable: Boolean,
  showsTable: Boolean,
  onShowTable: (Boolean) -> Unit,
  separator: Separator,
  detected: DelimiterChoice?,
  onSeparator: (Separator) -> Unit,
  hasHeader: Boolean,
  onHeader: (Boolean) -> Unit,
  isWrapped: Boolean,
  onWrapped: (Boolean) -> Unit,
) {
  Row(
    Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()).padding(horizontal = 12.dp, vertical = 4.dp),
    horizontalArrangement = Arrangement.spacedBy(8.dp),
    verticalAlignment = Alignment.CenterVertically,
  ) {
    if (canShowTable) {
      FilterChip(selected = showsTable, onClick = { onShowTable(true) }, label = { Text(stringResource(R.string.text_preview_view_table)) })
      FilterChip(selected = !showsTable, onClick = { onShowTable(false) }, label = { Text(stringResource(R.string.text_preview_view_text)) })
    }
    if (showsTable || (!canShowTable && (detected != null))) {
      SeparatorPicker(separator, detected, onSeparator)
      FilterChip(
        selected = hasHeader,
        onClick = { onHeader(!hasHeader) },
        label = { Text(stringResource(R.string.text_preview_first_row_header)) },
      )
    }
    FilterChip(selected = isWrapped, onClick = { onWrapped(!isWrapped) }, label = { Text(stringResource(R.string.text_preview_wrap)) })
  }
}

@Composable
private fun SeparatorPicker(separator: Separator, detected: DelimiterChoice?, onSeparator: (Separator) -> Unit) {
  var isOpen by remember { mutableStateOf(false) }
  var isCustomOpen by remember { mutableStateOf(false) }
  val label = separator.label(detected)
  Box {
    FilterChip(
      selected = separator != Separator.Auto,
      onClick = { isOpen = true },
      label = { Text(stringResource(R.string.text_preview_separator) + ": " + label) },
    )
    DropdownMenu(expanded = isOpen, onDismissRequest = { isOpen = false }) {
      for (option in Separator.options) {
        DropdownMenuItem(text = { Text(option.label(detected)) }, onClick = { isOpen = false; onSeparator(option) })
      }
      DropdownMenuItem(
        text = { Text(stringResource(R.string.text_preview_separator_custom)) },
        onClick = { isOpen = false; isCustomOpen = true },
      )
    }
  }
  if (isCustomOpen) {
    CustomSeparatorDialog(
      onDismiss = { isCustomOpen = false },
      onApply = { text -> isCustomOpen = false; onSeparator(Separator.Custom(text)) },
    )
  }
}

/** A separator the user types; the error sits at the field (forms.md). */
@Composable
private fun CustomSeparatorDialog(onDismiss: () -> Unit, onApply: (String) -> Unit) {
  var text by rememberSaveable { mutableStateOf("") }
  var hasTried by rememberSaveable { mutableStateOf(false) }
  val isValid = Separator.isUsable(text)
  val focus = remember { FocusRequester() }
  LaunchedEffect(Unit) { focus.requestFocus() }
  AlertDialog(
    onDismissRequest = onDismiss,
    title = { Text(stringResource(R.string.text_preview_separator)) },
    text = {
      OutlinedTextField(
        value = text,
        onValueChange = { text = it },
        label = { Text(stringResource(R.string.text_preview_separator_custom_label)) },
        singleLine = true,
        isError = hasTried && !isValid,
        supportingText = { if (hasTried && !isValid) Text(stringResource(R.string.text_preview_separator_custom_invalid)) },
        keyboardOptions = KeyboardOptions(imeAction = ImeAction.Done),
        keyboardActions = KeyboardActions(onDone = { if (isValid) onApply(text) else hasTried = true }),
        modifier = Modifier.focusRequester(focus),
      )
    },
    confirmButton = {
      TextButton(onClick = { if (isValid) onApply(text) else hasTried = true }) { Text(stringResource(R.string.text_preview_apply)) }
    },
    dismissButton = { TextButton(onClick = onDismiss) { Text(stringResource(R.string.text_preview_cancel)) } },
  )
}

@Composable
private fun FindBar(query: String, onQuery: (String) -> Unit, count: Int, current: Int, isCapped: Boolean, onStep: (Int) -> Unit) {
  val focus = remember { FocusRequester() }
  LaunchedEffect(Unit) { focus.requestFocus() }
  Row(Modifier.fillMaxWidth().padding(horizontal = 12.dp), verticalAlignment = Alignment.CenterVertically) {
    OutlinedTextField(
      value = query,
      onValueChange = onQuery,
      placeholder = { Text(stringResource(R.string.text_preview_find_hint)) },
      singleLine = true,
      keyboardOptions = KeyboardOptions(imeAction = ImeAction.Search),
      keyboardActions = KeyboardActions(onSearch = { onStep(1) }),
      modifier = Modifier.weight(1f).focusRequester(focus),
    )
    val summary = when {
      query.isEmpty() -> ""
      count == 0 -> stringResource(R.string.text_preview_find_none)
      isCapped -> stringResource(R.string.text_preview_find_count_capped, current + 1, count)
      else -> stringResource(R.string.text_preview_find_count, current + 1, count)
    }
    Text(summary, Modifier.padding(horizontal = 8.dp), style = MaterialTheme.typography.labelMedium)
    IconButton(onClick = { onStep(-1) }, enabled = count > 0) {
      Icon(Icons.Filled.KeyboardArrowUp, contentDescription = stringResource(R.string.text_preview_find_previous))
    }
    IconButton(onClick = { onStep(1) }, enabled = count > 0) {
      Icon(Icons.Filled.KeyboardArrowDown, contentDescription = stringResource(R.string.text_preview_find_next))
    }
  }
}

/** What the user should know about this view: a cut file, a refused table, repaired bytes. */
@Composable
private fun Notes(isTruncated: Boolean, encoding: PreviewEncoding, tableState: TableState, maybeTable: TableInfo?) {
  Column(Modifier.padding(horizontal = 16.dp)) {
    if (isTruncated) Note(stringResource(R.string.text_preview_truncated))
    if (encoding == PreviewEncoding.UTF8_REPAIRED) Note(stringResource(R.string.text_preview_encoding_repaired))
    if (tableState is TableState.Refused) {
      Note(
        if (tableState.arg != null) stringResource(tableState.message, tableState.arg) else stringResource(tableState.message),
      )
    }
    if (maybeTable?.hasDroppedColumns == true) Note(stringResource(R.string.text_preview_columns_dropped))
  }
}

@Composable
private fun Note(text: String) {
  Text(text, Modifier.padding(vertical = 2.dp), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
}

/** Lines in a virtualised list: a gutter with the line number, then the text (wrapped or scrolling sideways). */
@Composable
private fun TextView(
  preview: TextPreview,
  lineCount: Int,
  isWrapped: Boolean,
  matches: List<LineMatch>,
  current: LineMatch?,
  listState: LazyListState,
) {
  val byLine = remember(matches) { matches.groupBy { it.line.toInt() } }
  val sideways = rememberScrollState()
  val gutter = (lineCount.toString().length * GUTTER_DIGIT.value).dp + 16.dp
  val all = highlight(false)
  val now = highlight(true)
  LazyColumn(Modifier.fillMaxSize(), state = listState) {
    items(count = lineCount) { index ->
      val line = remember(preview, index) { preview.lines(index.toUInt(), 1u).firstOrNull().orEmpty() }
      val text = highlighted(line, byLine[index].orEmpty().map { it.start.toInt() to it.end.toInt() }, current?.takeIf { it.line.toInt() == index }?.let { it.start.toInt() }, all, now)
      Row(Modifier.fillMaxWidth()) {
        Text(
          (index + 1).toString(),
          Modifier.width(gutter).padding(end = 8.dp),
          textAlign = TextAlign.End,
          style = MaterialTheme.typography.bodySmall.copy(fontFamily = FontFamily.Monospace),
          color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        val style = MaterialTheme.typography.bodySmall.copy(fontFamily = FontFamily.Monospace, fontSize = 13.sp)
        if (isWrapped) {
          Text(text, Modifier.weight(1f).padding(end = 12.dp), style = style)
        } else {
          Box(Modifier.weight(1f).horizontalScroll(sideways)) { Text(text, style = style, softWrap = false, maxLines = 1) }
        }
      }
    }
  }
}

/** A table with a sticky header and row numbers; rows are drawn as they scroll into view. */
@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun TableView(
  preview: TextPreview,
  info: TableInfo,
  hasHeader: Boolean,
  isWrapped: Boolean,
  hits: List<CellHit>,
  current: CellHit?,
  listState: LazyListState,
) {
  val context = LocalContext.current
  val profile = remember(preview, info, hasHeader) { preview.tableProfile(hasHeader) }
  val sideways = rememberScrollState()
  val firstBody = if (hasHeader) 1 else 0
  val bodyRows = (info.rows.toInt() - firstBody).coerceAtLeast(0)
  val byCell = remember(hits) { hits.groupBy { it.row.toLong() shl 32 or it.column.toLong() } }
  val gutter = (info.rows.toString().length * GUTTER_DIGIT.value).dp + 16.dp
  val all = highlight(false)
  val now = highlight(true)
  var maybeMenuAt by remember { mutableStateOf<Pair<Int, Int>?>(null) }

  fun copy(text: String) {
    val clipboard = context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
    clipboard.setPrimaryClip(ClipData.newPlainText(null, text))
    Toast.makeText(context, R.string.text_preview_copied, Toast.LENGTH_SHORT).show()
  }

  @Composable
  fun RowCells(row: Int, cells: List<String>, isHeader: Boolean) {
    Row(Modifier.height(IntrinsicSize.Min)) {
      for (column in profile.indices) {
        val columnInfo = profile[column]
        val raw = cells.getOrNull(column).orEmpty()
        val shown = if (isHeader && raw.isEmpty()) stringResource(R.string.text_preview_header_cell, column + 1) else raw
        val ranges = byCell[row.toLong() shl 32 or column.toLong()].orEmpty()
        val text = highlighted(
          shown,
          ranges.map { it.start.toInt() to it.end.toInt() },
          current?.takeIf { it.row.toInt() == row && it.column.toInt() == column }?.start?.toInt(),
          all,
          now,
        )
        Box(
          Modifier
            .width(columnWidth(columnInfo))
            .fillMaxHeight()
            .combinedClickable(onClick = {}, onLongClick = { maybeMenuAt = row to column }, role = Role.Button)
            .padding(horizontal = CELL_PADDING, vertical = 6.dp)
            .semantics { contentDescription = shown },
        ) {
          Text(
            text,
            Modifier.fillMaxWidth(),
            textAlign = if (columnInfo.isNumeric && !isHeader) TextAlign.End else TextAlign.Start,
            maxLines = if (isWrapped) 6 else 1,
            overflow = TextOverflow.Ellipsis,
            fontWeight = if (isHeader) FontWeight.SemiBold else null,
            style = MaterialTheme.typography.bodySmall,
          )
          if (maybeMenuAt == row to column) {
            DropdownMenu(expanded = true, onDismissRequest = { maybeMenuAt = null }) {
              DropdownMenuItem(
                text = { Text(stringResource(R.string.text_preview_copy_cell)) },
                onClick = { maybeMenuAt = null; copy(preview.tableCell(row.toUInt(), column.toUInt())) },
              )
              DropdownMenuItem(
                text = { Text(stringResource(R.string.text_preview_copy_row)) },
                onClick = { maybeMenuAt = null; copy(preview.tableRowText(row.toUInt())) },
              )
            }
          }
        }
      }
    }
  }

  Column(Modifier.fillMaxSize()) {
    if (hasHeader) {
      val header = remember(preview, info) { preview.tableRows(0u, 1u).firstOrNull().orEmpty() }
      Row(Modifier.background(MaterialTheme.colorScheme.surfaceVariant)) {
        GutterSpace(gutter)
        Box(Modifier.weight(1f).horizontalScroll(sideways)) { RowCells(0, header, isHeader = true) }
      }
      HorizontalDivider()
    }
    LazyColumn(Modifier.fillMaxSize(), state = listState) {
      items(count = bodyRows) { index ->
        val row = index + firstBody
        val cells = remember(preview, info, row) { preview.tableRows(row.toUInt(), 1u).firstOrNull().orEmpty() }
        Row(Modifier.fillMaxWidth()) {
          Text(
            (row + 1).toString(),
            Modifier.width(gutter).padding(end = 8.dp, top = 6.dp),
            textAlign = TextAlign.End,
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
          )
          Box(Modifier.weight(1f).horizontalScroll(sideways)) { RowCells(row, cells, isHeader = false) }
        }
      }
    }
  }
}

@Composable
private fun GutterSpace(width: androidx.compose.ui.unit.Dp) {
  Box(Modifier.width(width))
}

private fun columnWidth(column: ColumnInfo) = (column.widthChars.toInt() * CHAR_WIDTH.value).dp + CELL_PADDING * 2

@Composable
private fun highlight(isCurrent: Boolean): SpanStyle {
  val color = MaterialTheme.colorScheme.tertiary
  return SpanStyle(background = color.copy(alpha = if (isCurrent) 0.75f else 0.35f))
}

/** [text] with the match ranges (UTF-16 offsets) marked; the one starting at [currentStart] stronger. */
private fun highlighted(text: String, ranges: List<Pair<Int, Int>>, currentStart: Int?, all: SpanStyle, now: SpanStyle): AnnotatedString {
  if (ranges.isEmpty()) return AnnotatedString(text)
  return buildAnnotatedString {
    append(text)
    for ((start, end) in ranges) {
      if (start < end && end <= text.length) addStyle(if (start == currentStart) now else all, start, end)
    }
  }
}

/** The separator the user chose for this type of file. */
private sealed interface Separator {
  data object Auto : Separator
  data class Picked(val delimiter: DelimiterChoice, val slug: String, @StringRes val label: Int) : Separator
  data class Custom(val text: String) : Separator

  val choice: DelimiterChoice?
    get() = when (this) {
      Auto -> null
      is Picked -> delimiter
      is Custom -> DelimiterChoice.Custom(text)
    }

  @Composable
  fun label(detected: DelimiterChoice?): String = when (this) {
    Auto -> stringResource(R.string.text_preview_separator_auto) + detected.let { d -> if (d == null) "" else " (" + nameOf(d) + ")" }
    is Picked -> stringResource(label)
    is Custom -> "\"$text\""
  }

  companion object {
    val options: List<Separator> = listOf(
      Auto,
      Picked(DelimiterChoice.Comma, "comma", R.string.text_preview_separator_comma),
      Picked(DelimiterChoice.Semicolon, "semicolon", R.string.text_preview_separator_semicolon),
      Picked(DelimiterChoice.Tab, "tab", R.string.text_preview_separator_tab),
      Picked(DelimiterChoice.Pipe, "pipe", R.string.text_preview_separator_pipe),
      Picked(DelimiterChoice.Colon, "colon", R.string.text_preview_separator_colon),
    )

    /** 1 to 8 bytes without line breaks or quotes, as the core accepts. */
    fun isUsable(text: String): Boolean =
      text.isNotEmpty() && text.toByteArray().size <= MAX_SEPARATOR_BYTES && text.none { it == '\n' || it == '\r' || it == '"' || it == '\'' }

    fun read(prefs: SharedPreferences, typeKey: String): Separator {
      val stored = prefs.getString("$typeKey.sep", null) ?: return Auto
      if (stored.startsWith("custom:")) return stored.removePrefix("custom:").takeIf(::isUsable)?.let(::Custom) ?: Auto
      return options.filterIsInstance<Picked>().firstOrNull { it.slug == stored } ?: Auto
    }

    fun write(prefs: SharedPreferences, typeKey: String, separator: Separator) {
      val value = when (separator) {
        Auto -> null
        is Picked -> separator.slug
        is Custom -> "custom:${separator.text}"
      }
      prefs.edit().apply { if (value == null) remove("$typeKey.sep") else putString("$typeKey.sep", value) }.apply()
    }
  }
}

@Composable
private fun nameOf(choice: DelimiterChoice): String = when (choice) {
  DelimiterChoice.Comma -> stringResource(R.string.text_preview_separator_comma)
  DelimiterChoice.Semicolon -> stringResource(R.string.text_preview_separator_semicolon)
  DelimiterChoice.Tab -> stringResource(R.string.text_preview_separator_tab)
  DelimiterChoice.Pipe -> stringResource(R.string.text_preview_separator_pipe)
  DelimiterChoice.Colon -> stringResource(R.string.text_preview_separator_colon)
  is DelimiterChoice.Custom -> "\"${choice.text}\""
}

/** The header switch as stored: `null` while the user hasn't touched it (the file's look decides). */
private object HeaderPrefs {
  fun read(prefs: SharedPreferences, typeKey: String): Boolean? =
    if (prefs.contains("$typeKey.header")) prefs.getBoolean("$typeKey.header", false) else null

  fun write(prefs: SharedPreferences, typeKey: String, hasHeader: Boolean) {
    prefs.edit().putBoolean("$typeKey.header", hasHeader).apply()
  }
}
