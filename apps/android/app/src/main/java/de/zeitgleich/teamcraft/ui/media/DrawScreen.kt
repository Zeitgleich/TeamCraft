package de.zeitgleich.teamcraft.ui.media

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.detectDragGestures
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.gestures.detectVerticalDragGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.material3.Button
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.IconToggleButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.lerp
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.ProgressBarRangeInfo
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.progressBarRangeInfo
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.setProgress
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import de.zeitgleich.teamcraft.ui.screens.PageLoadingIndicator
import de.zeitgleich.teamcraft.ui.screens.textKeyboard
import kotlinx.coroutines.launch
import de.zeitgleich.teamcraft.R

private val CANVAS_MARGIN = 12.dp

/** A point is added to a stroke once the finger moved this far: smooth lines, small drafts. */
private val POINT_STEP = 2.dp

/** The size slider's track: as wide as the thickest stroke reads at the top, a line at the bottom. */
private val WEDGE_TOP = 14.dp
private val WEDGE_BOTTOM = 2.dp

/** The colour slider: white, the hues, black. */
private val PALETTE = listOf(
  Color.White,
  Color(0xFFFF3B30),
  Color(0xFFFF9500),
  Color(0xFFFFCC00),
  Color(0xFF34C759),
  Color(0xFF00C7BE),
  Color(0xFF007AFF),
  Color(0xFFAF52DE),
  Color(0xFFFF2D55),
  Color.Black,
)

/** The drawing tools. */
private enum class DrawTool(val icon: Int, val label: Int) {
  Pen(R.drawable.ic_draw, R.string.media_tool_pen),
  Highlighter(R.drawable.ic_ink_highlighter, R.string.media_tool_highlighter),
  Text(R.drawable.ic_text_fields, R.string.media_tool_text),
  Sticker(R.drawable.ic_emoji_symbols, R.string.media_tool_sticker),
  Blur(R.drawable.ic_blur_on, R.string.media_tool_blur),
}

/**
 * Draws on a picture as in the common messengers (#1358): pen, highlighter, text, stickers and a
 * blur brush with "Blur faces" (found on the device), a colour slider and a size slider at the
 * sides, undo and redo, "Reset all". Text and stickers move with a drag while their tool is on;
 * the last one touched can be resized or removed.
 */
@Composable
fun DrawScreen(uri: String, initial: PictureEdit, onCancel: () -> Unit, onDone: (PictureEdit) -> Unit) {
  val maybePicture by rememberEditablePicture(uri)
  val history = rememberEditHistory(uri, initial)
  val edit = history.value
  var tool by rememberSaveable { mutableStateOf(DrawTool.Pen) }
  var colorPosition by rememberSaveable { mutableFloatStateOf(0.11f) }
  var sizePosition by rememberSaveable { mutableFloatStateOf(0.3f) }
  // The text or sticker last placed or touched (an index into the marks), for resizing and removal.
  var selected by remember { mutableIntStateOf(-1) }
  // Text being written: a new one (index -1) or one being changed.
  var maybeWriting by remember { mutableStateOf<Pair<Int, String>?>(null) }
  var isFindingFaces by remember { mutableStateOf(false) }
  val scope = rememberCoroutineScope()
  val color = paletteColor(colorPosition)
  val selectedMark = edit.marks.getOrNull(selected)?.takeIf { it is TextMark || it is StickerMark }

  fun setBlurFaces(isOn: Boolean) {
    val picture = maybePicture ?: return
    if (!isOn || edit.maybeFaces != null) {
      history.set(history.value.copy(isBlurringFaces = isOn))
      return
    }
    isFindingFaces = true
    scope.launch {
      val faces = PictureRenderer.findFaces(picture)
      isFindingFaces = false
      history.set(history.value.copy(isBlurringFaces = true, maybeFaces = faces))
    }
  }

  Box(Modifier.fillMaxSize().background(Color.Black)) {
    Column(Modifier.fillMaxSize().windowInsetsPadding(WindowInsets.safeDrawing)) {
      EditorTopBar(
        history,
        canReset = edit.marks.isNotEmpty() || edit.isBlurringFaces,
        onReset = {
          selected = -1
          history.set(edit.copy(marks = emptyList(), isBlurringFaces = false))
        },
        onCancel = onCancel,
      )
      Row(Modifier.weight(1f).fillMaxWidth()) {
        // Thick at the top, thin at the bottom, as the wedge of its track shows.
        VerticalSlider(
          value = 1f - sizePosition,
          label = stringResource(R.string.media_size),
          onValue = { slider ->
            val position = 1f - slider
            sizePosition = position
            // The selected text or sticker follows the size slider.
            selectedMark?.let { mark -> history.value = history.value.copy(marks = history.value.marks.toMutableList().also { it[selected] = resized(mark, position, maybePicture, edit) }) }
          },
          onStart = { if (selectedMark != null) history.record() },
          track = { SolidColor(Color.White.copy(alpha = 0.8f)) },
          isWedge = true,
          modifier = Modifier.width(40.dp).fillMaxHeight().padding(vertical = 24.dp),
        )
        Box(Modifier.weight(1f).fillMaxHeight(), contentAlignment = Alignment.Center) {
          val picture = maybePicture
          if (picture == null) {
            PageLoadingIndicator(color = Color.White)
          } else {
            DrawCanvas(
              picture = picture,
              edit = edit,
              tool = tool,
              color = color,
              sizePosition = sizePosition,
              onStart = history::record,
              onChange = { history.value = it },
              onSelect = { selected = it },
              onWrite = { index, text -> maybeWriting = index to text },
            )
          }
        }
        VerticalSlider(
          value = colorPosition,
          label = stringResource(R.string.media_color),
          onValue = { position ->
            colorPosition = position
            selectedMark?.let { mark -> history.value = history.value.copy(marks = history.value.marks.toMutableList().also { it[selected] = recolored(mark, paletteColor(position)) }) }
          },
          onStart = { if (selectedMark != null) history.record() },
          track = { Brush.verticalGradient(PALETTE) },
          thumbColor = color,
          modifier = Modifier.width(40.dp).fillMaxHeight().padding(vertical = 24.dp),
        )
      }
      ToolOptions(
        tool = tool,
        color = color,
        edit = edit,
        isFindingFaces = isFindingFaces,
        hasSelection = selectedMark != null,
        onBlurFaces = ::setBlurFaces,
        onSticker = { sticker ->
          val picture = maybePicture ?: return@ToolOptions
          history.set(edit.copy(marks = edit.marks + newSticker(sticker, color, sizePosition, picture, edit)))
          selected = history.value.marks.size - 1
        },
        onRemove = {
          history.set(edit.copy(marks = edit.marks.filterIndexed { index, _ -> index != selected }))
          selected = -1
        },
      )
      Row(Modifier.fillMaxWidth().padding(horizontal = 8.dp, vertical = 8.dp), verticalAlignment = Alignment.CenterVertically) {
        for (choice in DrawTool.entries) {
          IconToggleButton(
            checked = tool == choice,
            onCheckedChange = {
              tool = choice
              if (choice == DrawTool.Text) maybeWriting = -1 to ""
            },
          ) {
            Icon(painterResource(choice.icon), contentDescription = stringResource(choice.label), tint = if (tool == choice) ACTIVE_TINT else Color.White)
          }
        }
        Box(Modifier.weight(1f))
        Button(onClick = { onDone(edit) }) { Text(stringResource(R.string.picture_edit_done)) }
      }
    }
    maybeWriting?.let { (index, text) ->
      TextEntry(
        initial = text,
        color = color,
        onDone = { written ->
          maybeWriting = null
          val picture = maybePicture ?: return@TextEntry
          val current = history.value
          when {
            index >= 0 && written.isBlank() -> history.set(current.copy(marks = current.marks.filterIndexed { i, _ -> i != index }))
            index >= 0 -> history.set(current.copy(marks = current.marks.toMutableList().also { list -> (list[index] as? TextMark)?.let { list[index] = it.copy(text = written, color = color.toArgb()) } }))
            written.isNotBlank() -> {
              history.set(current.copy(marks = current.marks + newText(written, color, sizePosition, picture, current)))
              selected = history.value.marks.size - 1
            }
          }
        },
      )
    }
  }
}

/** The picture with its marks; touches draw, place or move depending on the tool. */
@Composable
private fun DrawCanvas(
  picture: EditablePicture,
  edit: PictureEdit,
  tool: DrawTool,
  color: Color,
  sizePosition: Float,
  onStart: () -> Unit,
  onChange: (PictureEdit) -> Unit,
  onSelect: (Int) -> Unit,
  onWrite: (Int, String) -> Unit,
) {
  val density = LocalDensity.current
  val step = with(density) { POINT_STEP.toPx() }
  val margin = with(density) { CANVAS_MARGIN.toPx() }
  val description = stringResource(R.string.media_draw_area)
  BoxWithConstraints(Modifier.fillMaxSize()) {
    // Remembered: a new one on every stroke point would restart the gestures keyed on it, and a
    // stroke would end after its first point.
    val width = constraints.maxWidth.toFloat()
    val height = constraints.maxHeight.toFloat()
    val fit = remember(picture, edit.crop, width, height, margin) {
      PictureFit.of(picture, edit.crop, isFramed = true, width, height, margin)
    }
    val current by rememberUpdatedState(edit)
    val currentTool by rememberUpdatedState(tool)
    val currentColor by rememberUpdatedState(color)
    val currentSize by rememberUpdatedState(sizePosition)
    EditedPictureCanvas(picture, edit, fit, Modifier.fillMaxSize())
    Box(
      Modifier
        .fillMaxSize()
        .semantics { contentDescription = description }
        .pointerInput(fit) {
          detectTapGestures { at ->
            val (x, y) = fit.toUnits(picture, at.x, at.y) ?: return@detectTapGestures
            val index = markAt(current.marks, x, y)
            when (currentTool) {
              DrawTool.Text -> {
                val maybeText = current.marks.getOrNull(index) as? TextMark
                onSelect(index)
                onWrite(if (maybeText != null) index else -1, maybeText?.text.orEmpty())
              }
              DrawTool.Sticker -> onSelect(index)
              DrawTool.Pen, DrawTool.Highlighter, DrawTool.Blur -> {
                onStart()
                val stroke = StrokeMark(strokeTool(currentTool), currentColor.toArgb(), strokeWidth(currentTool, currentSize, fit, picture, density.density), listOf(x, y))
                onChange(current.copy(marks = current.marks + stroke))
              }
            }
          }
        }
        .pointerInput(fit) {
          var moving = -1
          var last = Offset.Zero
          detectDragGestures(
            onDragStart = { at ->
              last = at
              val (x, y) = fit.toUnits(picture, at.x, at.y) ?: return@detectDragGestures
              onStart()
              if (currentTool == DrawTool.Text || currentTool == DrawTool.Sticker) {
                moving = markAt(current.marks, x, y)
                onSelect(moving)
              } else {
                moving = -1
                val stroke = StrokeMark(strokeTool(currentTool), currentColor.toArgb(), strokeWidth(currentTool, currentSize, fit, picture, density.density), listOf(x, y))
                onChange(current.copy(marks = current.marks + stroke))
              }
            },
            onDrag = { change, _ ->
              change.consume()
              val at = change.position
              val (x, y) = fit.toUnits(picture, at.x, at.y) ?: return@detectDragGestures
              val marks = current.marks.toMutableList()
              if (currentTool == DrawTool.Text || currentTool == DrawTool.Sticker) {
                val mark = marks.getOrNull(moving) ?: return@detectDragGestures
                marks[moving] = movedTo(mark, x, y)
              } else {
                if ((at - last).getDistance() < step) return@detectDragGestures
                val stroke = marks.lastOrNull() as? StrokeMark ?: return@detectDragGestures
                marks[marks.size - 1] = stroke.copy(points = stroke.points + listOf(x, y))
              }
              last = at
              onChange(current.copy(marks = marks))
            },
          )
        },
    )
  }
}

/** What the tool shows above the tool row: blur's face switch, the stickers, remove for a selection. */
@Composable
private fun ToolOptions(
  tool: DrawTool,
  color: Color,
  edit: PictureEdit,
  isFindingFaces: Boolean,
  hasSelection: Boolean,
  onBlurFaces: (Boolean) -> Unit,
  onSticker: (Sticker) -> Unit,
  onRemove: () -> Unit,
) {
  Row(Modifier.fillMaxWidth().height(56.dp).padding(horizontal = 12.dp), verticalAlignment = Alignment.CenterVertically) {
    when (tool) {
      DrawTool.Blur -> {
        Icon(painterResource(R.drawable.ic_face), contentDescription = null, tint = Color.White)
        Column(Modifier.weight(1f).padding(horizontal = 12.dp)) {
          Text(stringResource(R.string.media_blur_faces), color = Color.White, style = MaterialTheme.typography.bodyLarge)
          val note = when {
            isFindingFaces -> stringResource(R.string.media_blur_faces_finding)
            edit.isBlurringFaces && edit.maybeFaces?.isEmpty() == true -> stringResource(R.string.media_blur_faces_none)
            else -> stringResource(R.string.media_blur_hint)
          }
          Text(note, color = Color.White.copy(alpha = 0.7f), style = MaterialTheme.typography.bodySmall)
        }
        Switch(checked = edit.isBlurringFaces, onCheckedChange = onBlurFaces, enabled = !isFindingFaces)
      }
      DrawTool.Sticker -> {
        Row(Modifier.weight(1f), horizontalArrangement = Arrangement.SpaceEvenly) {
          for (sticker in Sticker.entries) {
            IconButton(onClick = { onSticker(sticker) }) {
              Icon(painterResource(PictureRenderer.stickerIcon(sticker)), contentDescription = stringResource(stickerLabel(sticker)), tint = color)
            }
          }
        }
      }
      else -> Box(Modifier.weight(1f))
    }
    if (hasSelection && (tool == DrawTool.Text || tool == DrawTool.Sticker)) {
      IconButton(onClick = onRemove) {
        Icon(painterResource(R.drawable.ic_delete), contentDescription = stringResource(R.string.media_remove_mark), tint = Color.White)
      }
    }
  }
}

/** Writing a text: large, in the chosen colour, over the dimmed picture. */
@Composable
private fun TextEntry(initial: String, color: Color, onDone: (String) -> Unit) {
  var text by remember { mutableStateOf(initial) }
  val focus = remember { FocusRequester() }
  LaunchedEffect(Unit) { focus.requestFocus() }
  // Dark enough that the editor's own top bar behind it doesn't read through its Done button.
  Box(Modifier.fillMaxSize().background(Color.Black.copy(alpha = 0.85f)).windowInsetsPadding(WindowInsets.safeDrawing).imePadding()) {
    TextButton(onClick = { onDone(text) }, modifier = Modifier.align(Alignment.TopEnd).padding(8.dp)) {
      Text(stringResource(R.string.picture_edit_done), color = Color.White)
    }
    BasicTextField(
      value = text,
      onValueChange = { text = it },
      textStyle = TextStyle(color = color, fontSize = 32.sp, fontWeight = FontWeight.Bold, textAlign = TextAlign.Center),
      cursorBrush = SolidColor(color),
      keyboardOptions = textKeyboard(),
      modifier = Modifier.align(Alignment.Center).fillMaxWidth().padding(24.dp).focusRequester(focus),
    )
  }
}

/** A slider standing up at the side of the picture: size on the left, colour on the right. */
@Composable
private fun VerticalSlider(
  value: Float,
  label: String,
  onValue: (Float) -> Unit,
  onStart: () -> Unit,
  track: () -> Brush,
  modifier: Modifier,
  thumbColor: Color = Color.White,
  isWedge: Boolean = false,
) {
  val current by rememberUpdatedState(value)
  // The gestures are set up once: they call the newest callbacks, not those of the first frame.
  val currentOnValue by rememberUpdatedState(onValue)
  val currentOnStart by rememberUpdatedState(onStart)
  Box(
    modifier
      .semantics {
        contentDescription = label
        progressBarRangeInfo = ProgressBarRangeInfo(value, 0f..1f)
        setProgress { target ->
          onStart()
          onValue(target.coerceIn(0f, 1f))
          true
        }
      }
      .pointerInput(Unit) {
        detectTapGestures { at ->
          currentOnStart()
          currentOnValue((at.y / size.height).coerceIn(0f, 1f))
        }
      }
      .pointerInput(Unit) {
        detectVerticalDragGestures(onDragStart = { at ->
          currentOnStart()
          currentOnValue((at.y / size.height).coerceIn(0f, 1f))
        }) { change, amount ->
          change.consume()
          currentOnValue((current + amount / size.height).coerceIn(0f, 1f))
        }
      },
    contentAlignment = Alignment.TopCenter,
  ) {
    if (isWedge) {
      Canvas(Modifier.width(WEDGE_TOP).fillMaxHeight()) {
        val top = size.width
        val bottom = WEDGE_BOTTOM.toPx()
        val wedge = Path().apply {
          moveTo(size.width / 2 - top / 2, 0f)
          lineTo(size.width / 2 + top / 2, 0f)
          lineTo(size.width / 2 + bottom / 2, size.height)
          lineTo(size.width / 2 - bottom / 2, size.height)
          close()
        }
        drawPath(wedge, track())
      }
    } else {
      Box(Modifier.width(6.dp).fillMaxHeight().background(track(), RoundedCornerShape(3.dp)))
    }
    Canvas(Modifier.fillMaxSize()) {
      val y = value * size.height
      drawCircle(Color.White, radius = 12.dp.toPx(), center = Offset(size.width / 2, y))
      drawCircle(thumbColor, radius = 9.dp.toPx(), center = Offset(size.width / 2, y))
    }
  }
}

private fun paletteColor(position: Float): Color {
  val scaled = position.coerceIn(0f, 1f) * (PALETTE.size - 1)
  val index = scaled.toInt().coerceAtMost(PALETTE.size - 2)
  return lerp(PALETTE[index], PALETTE[index + 1], scaled - index)
}

private fun strokeTool(tool: DrawTool): StrokeTool = when (tool) {
  DrawTool.Highlighter -> StrokeTool.Highlighter
  DrawTool.Blur -> StrokeTool.Blur
  else -> StrokeTool.Pen
}

/** The stroke width in units for the size slider's position: wider for the highlighter and the blur. */
private fun strokeWidth(tool: DrawTool, position: Float, fit: PictureFit, picture: EditablePicture, density: Float): Float {
  val (smallest, largest) = when (tool) {
    DrawTool.Highlighter -> 12f to 64f
    DrawTool.Blur -> 24f to 96f
    else -> 3f to 40f
  }
  val viewWidth = (smallest + (largest - smallest) * position) * density
  return viewWidth / fit.pixelsPerUnit(picture)
}

/** Text and stickers: their size in units for the slider's position, relative to the cropped picture's view. */
private fun markSize(isText: Boolean, position: Float, picture: EditablePicture, edit: PictureEdit): Float {
  val (width, height) = PictureGeometry.outputSize(picture.width, picture.height, edit.crop)
  val shown = maxOf(width, height) / picture.unit
  val share = if (isText) 0.03f + 0.12f * position else 0.08f + 0.32f * position
  return shown * share
}

private fun newText(text: String, color: Color, position: Float, picture: EditablePicture, edit: PictureEdit): TextMark {
  val (x, y) = viewCentre(picture, edit)
  return TextMark(text, color.toArgb(), x, y, markSize(true, position, picture, edit), PictureRenderer.upright(PictureRenderer.transform(picture.width, picture.height, edit.crop)))
}

private fun newSticker(sticker: Sticker, color: Color, position: Float, picture: EditablePicture, edit: PictureEdit): StickerMark {
  val (x, y) = viewCentre(picture, edit)
  return StickerMark(sticker, color.toArgb(), x, y, markSize(false, position, picture, edit), PictureRenderer.upright(PictureRenderer.transform(picture.width, picture.height, edit.crop)))
}

/** The middle of the cropped picture, in units. */
private fun viewCentre(picture: EditablePicture, edit: PictureEdit): Pair<Float, Float> {
  val (width, height) = PictureGeometry.outputSize(picture.width, picture.height, edit.crop)
  val inverse = android.graphics.Matrix()
  PictureRenderer.transform(picture.width, picture.height, edit.crop).invert(inverse)
  val point = floatArrayOf(width / 2, height / 2)
  inverse.mapPoints(point)
  return point[0] / picture.unit to point[1] / picture.unit
}

private fun resized(mark: Mark, position: Float, maybePicture: EditablePicture?, edit: PictureEdit): Mark {
  val picture = maybePicture ?: return mark
  return when (mark) {
    is TextMark -> mark.copy(size = markSize(true, position, picture, edit))
    is StickerMark -> mark.copy(size = markSize(false, position, picture, edit))
    is StrokeMark -> mark
  }
}

private fun recolored(mark: Mark, color: Color): Mark = when (mark) {
  is TextMark -> mark.copy(color = color.toArgb())
  is StickerMark -> mark.copy(color = color.toArgb())
  is StrokeMark -> mark
}

private fun movedTo(mark: Mark, x: Float, y: Float): Mark = when (mark) {
  is TextMark -> mark.copy(x = x, y = y)
  is StickerMark -> mark.copy(x = x, y = y)
  is StrokeMark -> mark
}

/** The topmost text or sticker under a point (in units), or -1. */
private fun markAt(marks: List<Mark>, x: Float, y: Float): Int {
  for (index in marks.indices.reversed()) {
    val mark = marks[index]
    val (centreX, centreY, upright) = when (mark) {
      is TextMark -> Triple(mark.x, mark.y, mark.upright)
      is StickerMark -> Triple(mark.x, mark.y, mark.upright)
      is StrokeMark -> continue
    }
    // Into the mark's own frame: `upright` is a rotation (and mirror), so its transpose undoes it.
    val dx = x - centreX
    val dy = y - centreY
    val localX = upright[0] * dx + upright[2] * dy
    val localY = upright[1] * dx + upright[3] * dy
    val (halfWidth, halfHeight) = PictureRenderer.halfExtent(mark)
    if (kotlin.math.abs(localX) <= halfWidth && kotlin.math.abs(localY) <= halfHeight) return index
  }
  return -1
}

private fun stickerLabel(sticker: Sticker): Int = when (sticker) {
  Sticker.Arrow -> R.string.media_sticker_arrow
  Sticker.Circle -> R.string.media_sticker_circle
  Sticker.Square -> R.string.media_sticker_square
  Sticker.Check -> R.string.media_sticker_check
  Sticker.Star -> R.string.media_sticker_star
  Sticker.Heart -> R.string.media_sticker_heart
  Sticker.Question -> R.string.media_sticker_question
}
