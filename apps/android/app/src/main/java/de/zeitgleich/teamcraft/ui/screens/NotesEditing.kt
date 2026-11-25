package de.zeitgleich.teamcraft.ui.screens

import android.text.format.DateUtils
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material3.Button
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.LocalContentColor
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.takeOrElse
import androidx.compose.ui.input.key.Key
import androidx.compose.ui.input.key.KeyEventType
import androidx.compose.ui.input.key.key
import androidx.compose.ui.input.key.onPreviewKeyEvent
import androidx.compose.ui.input.key.type
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalFocusManager
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.TextRange
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.ui.unit.dp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LifecycleEventEffect
import androidx.lifecycle.viewModelScope
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.FileInfo
import de.zeitgleich.teamcraft.ui.MainViewModel
import de.zeitgleich.teamcraft.ui.theme.Spacing
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import uniffi.teamcraft_ffi.CanvasBlockItem
import uniffi.teamcraft_ffi.CanvasBlockKind
import uniffi.teamcraft_ffi.CanvasEdit
import uniffi.teamcraft_ffi.CanvasFocusItem
import uniffi.teamcraft_ffi.CanvasPageEditor
import uniffi.teamcraft_ffi.CanvasPageItem
import uniffi.teamcraft_ffi.CanvasVersionItem
import uniffi.teamcraft_ffi.CanvasVersionKey
import uniffi.teamcraft_ffi.canvasCaretAfter

/** The kinds a block can be turned into from its menu, in the menu's order; a file block is made from a file. */
private val TYPED_KINDS = listOf(
  CanvasBlockKind.PARAGRAPH,
  CanvasBlockKind.HEADING,
  CanvasBlockKind.HEADING2,
  CanvasBlockKind.HEADING3,
  CanvasBlockKind.BULLET,
  CanvasBlockKind.NUMBERED,
  CanvasBlockKind.CHECK,
  CanvasBlockKind.QUOTE,
  CanvasBlockKind.CODE,
)

/**
 * Editing a page of the notes on the phone (#2361, e2ee.md §4.11): the core's editor of the page
 * (the one desktop and web run), and which block the user types in. Every edit goes to the core in
 * order and comes back as the page with it, at once; the core says when the edits of a burst of
 * typing go out ([CanvasEditOutcome.flushAfterMs]), and they go out at once when the page is left
 * or the app goes to the background. The work runs in the view model's scope, so an edit made just
 * before the sheet closed isn't lost.
 */
@Stable
class NotesEditing internal constructor(
  val maybeEditor: CanvasPageEditor?,
  private val viewModel: MainViewModel,
  private val onPage: (CanvasPageItem) -> Unit,
) {
  /** The block the user types in and where the caret goes when it opens; `null` while nobody types. */
  var maybeFocus by mutableStateOf<CanvasFocusItem?>(null)
    private set
  var canUndo by mutableStateOf(false)
    private set
  var canRedo by mutableStateOf(false)
    private set

  /** Edits handed to the core whose answer hasn't come: a page read meanwhile may not have them yet. */
  var inFlight by mutableIntStateOf(0)
    private set

  private val lock = Mutex()
  private var maybeFlush: Job? = null

  fun edit(edit: CanvasEdit) {
    val editor = maybeEditor ?: return
    inFlight += 1
    viewModel.viewModelScope.launch {
      lock.withLock {
        val maybeOutcome = viewModel.canvasEdit(editor, edit)
        inFlight -= 1
        val outcome = maybeOutcome ?: return@withLock
        onPage(outcome.page)
        canUndo = outcome.canUndo
        canRedo = outcome.canRedo
        outcome.maybeFocus?.let { maybeFocus = it }
        scheduleFlush(outcome.flushAfterMs?.toLong())
      }
    }
  }

  /** The user taps a block to type in it, the caret at [caret] (UTF-16 units). */
  fun startEditing(blockId: String, caret: Int) {
    maybeFocus = CanvasFocusItem(blockId, caret.coerceAtLeast(0).toUInt())
  }

  /** Done typing: the waiting edits go out now. */
  fun stopEditing() {
    maybeFocus = null
    flushNow()
  }

  fun flushNow() {
    val editor = maybeEditor ?: return
    maybeFlush?.cancel()
    viewModel.viewModelScope.launch { lock.withLock { viewModel.flushCanvasEdits(editor) } }
  }

  /** The page is left: the waiting edits go out and the core lets go of the editor, after the edits still on their way. */
  internal fun close() {
    val editor = maybeEditor ?: return
    maybeFlush?.cancel()
    viewModel.viewModelScope.launch {
      lock.withLock { viewModel.closeCanvasEditor(editor) }
      runCatching { editor.close() }
    }
  }

  private fun scheduleFlush(maybeAfterMs: Long?) {
    val editor = maybeEditor ?: return
    maybeFlush?.cancel()
    val afterMs = maybeAfterMs ?: return
    maybeFlush = viewModel.viewModelScope.launch {
      delay(afterMs)
      lock.withLock { viewModel.flushCanvasEdits(editor) }
    }
  }
}

/** The editing of the shown page; a new one, with an editor of its own, for every page. */
@Composable
fun rememberNotesEditing(
  viewModel: MainViewModel,
  conversationId: String,
  maybePageId: String?,
  onPage: (CanvasPageItem) -> Unit,
): NotesEditing {
  val currentOnPage by rememberUpdatedState(onPage)
  val editing = remember(conversationId, maybePageId) {
    // An editor names its page only; a page ID beyond the rules gets none and the page shows read-only.
    val maybeEditor = runCatching { CanvasPageEditor(conversationId, maybePageId) }.getOrNull()
    NotesEditing(maybeEditor, viewModel) { currentOnPage(it) }
  }
  DisposableEffect(editing) { onDispose { editing.close() } }
  LifecycleEventEffect(Lifecycle.Event.ON_STOP) { editing.flushNow() }
  return editing
}

/** How a block's text shows and its check box behaves while the page can be edited (#2361). */
internal class BlockEditing(
  val text: @Composable (CanvasBlockItem, TextStyle, Modifier) -> Unit,
  val onCheck: (CanvasBlockItem, Boolean) -> Unit,
)

/** Set while the page can be edited; the blocks' rows read it ([NotesBlockContent]). */
internal val LocalBlockEditing = staticCompositionLocalOf<BlockEditing?> { null }

/** The sheet's title with undo, redo, the version history and, while typing, "Done". */
@Composable
internal fun NotesTitleRow(canEdit: Boolean, editing: NotesEditing, onHistory: () -> Unit) {
  val focusManager = LocalFocusManager.current
  Row(verticalAlignment = Alignment.CenterVertically) {
    Text(
      stringResource(R.string.channel_notes_title),
      style = MaterialTheme.typography.titleMedium,
      modifier = Modifier.weight(1f).semantics { heading() },
    )
    if (canEdit) {
      IconButton(onClick = { editing.edit(CanvasEdit.Undo) }, enabled = editing.canUndo) {
        Icon(painterResource(R.drawable.ic_undo), contentDescription = stringResource(R.string.channel_notes_undo))
      }
      IconButton(onClick = { editing.edit(CanvasEdit.Redo) }, enabled = editing.canRedo) {
        Icon(painterResource(R.drawable.ic_redo), contentDescription = stringResource(R.string.channel_notes_redo))
      }
    }
    IconButton(onClick = onHistory) {
      Icon(painterResource(R.drawable.ic_history), contentDescription = stringResource(R.string.channel_notes_history))
    }
    if (editing.maybeFocus != null) {
      TextButton(onClick = {
        focusManager.clearFocus()
        editing.stopEditing()
      }) { Text(stringResource(R.string.channel_notes_done)) }
    }
  }
}

/** A block of a page the user may edit: tap its text to type, its menu changes, moves or removes it. */
@Composable
internal fun EditableNotesBlock(
  block: CanvasBlockItem,
  blocks: List<CanvasBlockItem>,
  editing: NotesEditing,
  files: List<FileInfo>,
  maybeMeId: String?,
  onOpenFile: (FileInfo) -> Unit,
) {
  val isTyping = editing.maybeFocus?.blockId == block.id
  val blockEditing = remember(editing) {
    BlockEditing(
      text = { shown, style, modifier -> EditableBlockText(shown, style, modifier, editing) },
      onCheck = { shown, isChecked -> editing.edit(CanvasEdit.SetChecked(shown.id, isChecked)) },
    )
  }
  Row(verticalAlignment = Alignment.Top) {
    Box(Modifier.weight(1f)) {
      CompositionLocalProvider(LocalBlockEditing provides blockEditing) {
        NotesBlock(block, files, maybeMeId, onOpenFile)
      }
    }
    // The menu of the block typed in, and of a file block, which isn't typed in.
    if (isTyping || block.kind == CanvasBlockKind.FILE) {
      BlockMenu(block, blocks, editing)
    }
  }
}

/** A block's text: formatted until tapped, then a field the user types in. */
@Composable
private fun EditableBlockText(block: CanvasBlockItem, style: TextStyle, modifier: Modifier, editing: NotesEditing) {
  val maybeFocus = editing.maybeFocus
  if (maybeFocus?.blockId == block.id) {
    BlockTextField(block, style, modifier, editing, maybeFocus.caret.toInt())
    return
  }
  val shown = block.runs.joinToString("") { it.text }
  val label = stringResource(R.string.channel_notes_edit_block)
  val tappable = modifier.fillMaxWidth().heightIn(min = 24.dp).clickable(onClickLabel = label) {
    editing.startEditing(block.id, shown.length)
  }
  if (shown.isEmpty()) {
    // An empty block still takes a tap.
    Text(
      stringResource(R.string.channel_notes_empty_block),
      style = style.copy(color = MaterialTheme.colorScheme.onSurfaceVariant),
      modifier = tappable,
    )
  } else {
    FormattedBlockText(block, style, tappable)
  }
}

/**
 * The field of the block the user types in. Its text is the user's while they type; a change of the
 * block from elsewhere (another member) is taken in once the own edits got their answer, the caret
 * kept where it was ([canvasCaretAfter]). Enter splits the block (in code it is a line break),
 * Backspace at the start joins it with the one above.
 */
@Composable
private fun BlockTextField(block: CanvasBlockItem, style: TextStyle, modifier: Modifier, editing: NotesEditing, caret: Int) {
  val shown = block.runs.joinToString("") { it.text }
  val isCode = block.kind == CanvasBlockKind.CODE
  var value by remember(block.id) { mutableStateOf(TextFieldValue(shown, TextRange(caret.coerceIn(0, shown.length)))) }
  // The block's text as the own edits left it: what the next one starts from.
  var synced by remember(block.id) { mutableStateOf(shown) }
  LaunchedEffect(shown, editing.inFlight) {
    if (editing.inFlight == 0 && shown != synced && value.text == synced) {
      val moved = canvasCaretAfter(synced, shown, value.selection.start.toUInt()).toInt()
      value = TextFieldValue(shown, TextRange(moved.coerceIn(0, shown.length)))
      synced = shown
    }
  }
  val requester = remember { FocusRequester() }
  LaunchedEffect(block.id) { requester.requestFocus() }
  val color = style.color.takeOrElse { LocalContentColor.current }
  BasicTextField(
    value = value,
    onValueChange = { next ->
      if (next.text == value.text) {
        value = next
        return@BasicTextField
      }
      val lineBreak = if (isCode) -1 else next.text.indexOf('\n')
      if (lineBreak >= 0) {
        // Enter: the first line break splits the block; further ones (a paste) become spaces.
        val text = next.text.removeRange(lineBreak, lineBreak + 1).replace('\n', ' ')
        editing.edit(CanvasEdit.Enter(block.id, synced, text, lineBreak.toUInt()))
        synced = text
        value = TextFieldValue(text, TextRange(lineBreak))
      } else {
        editing.edit(CanvasEdit.Type(block.id, synced, next.text))
        synced = next.text
        value = next
      }
    },
    textStyle = style.copy(color = color),
    cursorBrush = SolidColor(MaterialTheme.colorScheme.primary),
    modifier = modifier.fillMaxWidth().focusRequester(requester).onPreviewKeyEvent { event ->
      val isBackspaceAtStart = event.type == KeyEventType.KeyDown && event.key == Key.Backspace && value.selection == TextRange(0)
      if (isBackspaceAtStart) editing.edit(CanvasEdit.BackspaceAtStart(block.id))
      isBackspaceAtStart
    },
  )
}

/** A block's menu: its kind, moving it a place up or down, removing it (undo brings it back). */
@Composable
private fun BlockMenu(block: CanvasBlockItem, blocks: List<CanvasBlockItem>, editing: NotesEditing) {
  var isOpen by remember { mutableStateOf(false) }
  val index = blocks.indexOfFirst { it.id == block.id }
  Box {
    IconButton(onClick = { isOpen = true }) {
      Icon(Icons.Filled.MoreVert, contentDescription = stringResource(R.string.channel_notes_block_actions))
    }
    DropdownMenu(expanded = isOpen, onDismissRequest = { isOpen = false }) {
      if (block.kind != CanvasBlockKind.FILE) {
        for (kind in TYPED_KINDS) {
          DropdownMenuItem(
            text = { Text(stringResource(kindLabel(kind))) },
            trailingIcon = { if (kind == block.kind) Icon(Icons.Filled.Check, contentDescription = null) },
            onClick = {
              isOpen = false
              if (kind != block.kind) editing.edit(CanvasEdit.SetKind(block.id, kind))
            },
          )
        }
        HorizontalDivider()
      }
      DropdownMenuItem(
        text = { Text(stringResource(R.string.channel_notes_move_up)) },
        enabled = index > 0,
        onClick = {
          isOpen = false
          editing.edit(CanvasEdit.MoveUp(block.id))
        },
      )
      DropdownMenuItem(
        text = { Text(stringResource(R.string.channel_notes_move_down)) },
        enabled = index in 0 until blocks.lastIndex,
        onClick = {
          isOpen = false
          editing.edit(CanvasEdit.MoveDown(block.id))
        },
      )
      DropdownMenuItem(
        text = { Text(stringResource(R.string.channel_notes_delete_block)) },
        leadingIcon = { Icon(painterResource(R.drawable.ic_delete), contentDescription = null) },
        onClick = {
          isOpen = false
          editing.stopEditing()
          editing.edit(CanvasEdit.Remove(block.id))
        },
      )
    }
  }
}

private fun kindLabel(kind: CanvasBlockKind): Int = when (kind) {
  CanvasBlockKind.PARAGRAPH, CanvasBlockKind.FILE -> R.string.channel_notes_kind_paragraph
  CanvasBlockKind.HEADING -> R.string.channel_notes_kind_heading
  CanvasBlockKind.HEADING2 -> R.string.channel_notes_kind_heading2
  CanvasBlockKind.HEADING3 -> R.string.channel_notes_kind_heading3
  CanvasBlockKind.BULLET -> R.string.channel_notes_kind_bullet
  CanvasBlockKind.NUMBERED -> R.string.channel_notes_kind_numbered
  CanvasBlockKind.CHECK -> R.string.channel_notes_kind_check
  CanvasBlockKind.QUOTE -> R.string.channel_notes_kind_quote
  CanvasBlockKind.CODE -> R.string.channel_notes_kind_code
}

/**
 * A page's version history as this device holds it (#2361, e2ee.md §4.11): who changed it and when,
 * newest first; a version opens read-only and, for who may edit, is restored as a new change on top.
 */
@Composable
internal fun NotesHistory(
  viewModel: MainViewModel,
  conversationId: String,
  maybePageId: String?,
  changes: Long,
  files: List<FileInfo>,
  maybeMeId: String?,
  canRestore: Boolean,
  onRestore: (CanvasVersionKey) -> Unit,
  onClose: () -> Unit,
) {
  val context = LocalContext.current
  var maybeVersions by remember(conversationId, maybePageId) { mutableStateOf<List<CanvasVersionItem>?>(null) }
  LaunchedEffect(conversationId, maybePageId, changes) {
    maybeVersions = viewModel.canvasVersions(conversationId, maybePageId) ?: maybeVersions
  }
  var maybeSelected by remember(conversationId, maybePageId) { mutableStateOf<CanvasVersionItem?>(null) }
  val someone = stringResource(R.string.channel_notes_history_someone)
  val describe = { version: CanvasVersionItem ->
    DateUtils.formatDateRange(
      context,
      version.key.fromMs,
      version.key.untilMs,
      DateUtils.FORMAT_SHOW_DATE or DateUtils.FORMAT_SHOW_TIME or DateUtils.FORMAT_ABBREV_MONTH,
    )
  }
  LazyColumn(
    Modifier.fillMaxSize().padding(horizontal = Spacing.l).padding(bottom = Spacing.xxl),
    verticalArrangement = Arrangement.spacedBy(Spacing.s),
  ) {
    item {
      Row(verticalAlignment = Alignment.CenterVertically) {
        val selected = maybeSelected
        IconButton(onClick = { if (selected != null) maybeSelected = null else onClose() }) {
          Icon(
            Icons.AutoMirrored.Filled.ArrowBack,
            contentDescription = stringResource(if (selected != null) R.string.channel_notes_history_all else R.string.channel_notes_history_close),
          )
        }
        Text(
          stringResource(R.string.channel_notes_history),
          style = MaterialTheme.typography.titleMedium,
          modifier = Modifier.semantics { heading() },
        )
      }
    }
    val selected = maybeSelected
    if (selected == null) {
      item { NotesHint(stringResource(R.string.channel_notes_history_hint)) }
      val versions = maybeVersions
      when {
        versions == null -> item { PageLoadingIndicator(Modifier.padding(Spacing.l)) }
        versions.isEmpty() -> item { NotesHint(stringResource(R.string.channel_notes_history_empty)) }
        else -> items(versions, key = { "${it.key.userId}-${it.key.fromMs}-${it.key.untilMs}" }) { version ->
          ListItem(
            headlineContent = { Text(version.displayName.ifBlank { someone }) },
            supportingContent = { Text(describe(version)) },
            modifier = Modifier.clickable { maybeSelected = version },
          )
        }
      }
      return@LazyColumn
    }
    item {
      Text(
        "${selected.displayName.ifBlank { someone }} · ${describe(selected)}",
        style = MaterialTheme.typography.bodyMedium,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
      )
    }
    if (canRestore) {
      item { Button(onClick = { onRestore(selected.key) }) { Text(stringResource(R.string.channel_notes_history_restore)) } }
    }
    item {
      // Empty while it loads; `null` once the core says this device no longer has the version.
      val maybeBlocks by produceState<List<CanvasBlockItem>?>(emptyList(), selected, changes) {
        value = viewModel.canvasVersionPage(conversationId, maybePageId, selected.key)
      }
      VersionBlocks(maybeBlocks, files, maybeMeId)
    }
  }
}

/** A version's page, read-only; says so when the version is no longer on this device. */
@Composable
private fun VersionBlocks(maybeBlocks: List<CanvasBlockItem>?, files: List<FileInfo>, maybeMeId: String?) {
  val blocks = maybeBlocks ?: run {
    NotesHint(stringResource(R.string.channel_notes_history_gone))
    return
  }
  Column(verticalArrangement = Arrangement.spacedBy(Spacing.s)) {
    for (block in blocks) {
      key(block.id) { NotesBlock(block, files, maybeMeId, onOpenFile = {}) }
    }
  }
}
