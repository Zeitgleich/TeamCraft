package de.zeitgleich.teamcraft.ui.screens

import android.graphics.Bitmap
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.detectDragGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.material3.Button
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.drawscope.DrawScope
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.PickedFile
import de.zeitgleich.teamcraft.ui.CropCorner
import de.zeitgleich.teamcraft.ui.CropFrame
import de.zeitgleich.teamcraft.ui.MainViewModel
import de.zeitgleich.teamcraft.ui.PictureEdits
import de.zeitgleich.teamcraft.ui.Pictures

/** Longest side the picture is shown at while editing: sharp on a phone, cheap to turn. */
private const val PREVIEW_SIDE_PX = 1600

/** How close to a corner a drag must start to move that corner. */
private val CORNER_REACH = 40.dp
private val FRAME_MARGIN = 24.dp
private val HANDLE_LENGTH = 22.dp
private const val OUTSIDE_DIM = 0.55f

/**
 * Edits a picture before it is sent, as the common messengers do: turn it a quarter at a time and
 * crop it by dragging the frame's corners or the frame itself. Done stages the edited picture in
 * place of the original; Cancel leaves the original. Dark, whatever the theme, like a photo viewer.
 */
@Composable
fun PictureEditor(viewModel: MainViewModel, file: PickedFile, onCancel: () -> Unit, onDone: (PickedFile) -> Unit) {
  var maybeOriginal by remember(file.path) { mutableStateOf<Bitmap?>(null) }
  var turns by remember(file.path) { mutableIntStateOf(0) }
  var frame by remember(file.path) { mutableStateOf(CropFrame()) }
  var isSaving by remember { mutableStateOf(false) }
  var isFailed by remember { mutableStateOf(false) }
  val scope = rememberCoroutineScope()
  LaunchedEffect(file.path) {
    maybeOriginal = withContext(Dispatchers.IO) { Pictures.decodeUpright(file.path, PREVIEW_SIDE_PX) }
    if (maybeOriginal == null) isFailed = true
  }
  var maybeShown by remember(file.path) { mutableStateOf<ImageBitmap?>(null) }
  LaunchedEffect(maybeOriginal, turns) {
    maybeShown = maybeOriginal?.let { original -> withContext(Dispatchers.Default) { PictureEdits.turned(original, turns).asImageBitmap() } }
  }
  Dialog(onDismissRequest = onCancel, properties = DialogProperties(usePlatformDefaultWidth = false, decorFitsSystemWindows = false)) {
    Column(Modifier.fillMaxSize().background(Color.Black).windowInsetsPadding(WindowInsets.safeDrawing)) {
      Box(Modifier.weight(1f).fillMaxWidth(), contentAlignment = Alignment.Center) {
        val shown = maybeShown
        when {
          isFailed -> Text(stringResource(R.string.picture_edit_failed), color = Color.White)
          shown == null -> PageLoadingIndicator(color = Color.White)
          else -> CropCanvas(shown, frame, onFrame = { frame = it })
        }
      }
      Row(
        Modifier.fillMaxWidth().padding(horizontal = 8.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.SpaceBetween,
      ) {
        TextButton(onClick = onCancel) { Text(stringResource(R.string.cancel), color = Color.White) }
        Row(verticalAlignment = Alignment.CenterVertically) {
          IconButton(
            onClick = {
              turns = (turns + 1).mod(4)
              frame = frame.turnedLeft()
            },
            enabled = maybeShown != null && !isSaving,
          ) {
            Icon(painterResource(R.drawable.ic_rotate_left), contentDescription = stringResource(R.string.picture_rotate), tint = Color.White)
          }
          TextButton(
            onClick = {
              turns = 0
              frame = CropFrame()
            },
            enabled = (turns != 0 || !frame.isWhole) && !isSaving,
          ) { Text(stringResource(R.string.picture_reset), color = Color.White) }
        }
        Button(
          onClick = {
            if (turns == 0 && frame.isWhole) {
              onCancel()
              return@Button
            }
            isSaving = true
            scope.launch {
              val maybeEdited = PictureEdits.render(file.path, file.mediaType, turns, frame)?.let { (bytes, type) ->
                viewModel.stageEditedPicture(PictureEdits.editedName(file.fileName, file.mediaType), type, bytes)
              }
              isSaving = false
              if (maybeEdited == null) isFailed = true else onDone(maybeEdited)
            }
          },
          enabled = maybeShown != null && !isSaving,
        ) { Text(stringResource(R.string.picture_edit_done)) }
      }
    }
  }
}

/** What a drag that started at a point moves: a corner, the whole frame, or nothing. */
private sealed interface CropDrag {
  data class Corner(val corner: CropCorner) : CropDrag
  data object Frame : CropDrag
  data object Nothing : CropDrag
}

/** The picture fitted into the space, the area outside the frame dimmed, the frame with its corners. */
@Composable
private fun CropCanvas(image: ImageBitmap, frame: CropFrame, onFrame: (CropFrame) -> Unit) {
  val density = LocalDensity.current
  val description = stringResource(R.string.picture_crop_area)
  BoxWithConstraints(Modifier.fillMaxSize()) {
    val margin = with(density) { FRAME_MARGIN.toPx() }
    val area = Size(with(density) { maxWidth.toPx() } - margin * 2, with(density) { maxHeight.toPx() } - margin * 2)
    val scale = minOf(area.width / image.width, area.height / image.height)
    val drawn = Size(image.width * scale, image.height * scale)
    val picture = Rect(Offset(margin + (area.width - drawn.width) / 2, margin + (area.height - drawn.height) / 2), drawn)
    val reach = with(density) { CORNER_REACH.toPx() }
    // The newest frame inside the gesture: the lambda is set up once per picture size.
    val current by rememberUpdatedState(frame)
    Canvas(
      Modifier
        .fillMaxSize()
        .semantics { contentDescription = description }
        .pointerInput(picture) {
          var drag: CropDrag = CropDrag.Nothing
          detectDragGestures(
            onDragStart = { start -> drag = dragAt(start, frameRect(picture, current), reach) },
            onDrag = { change, amount ->
              change.consume()
              val dx = amount.x / picture.width
              val dy = amount.y / picture.height
              when (val active = drag) {
                is CropDrag.Corner -> onFrame(current.dragged(active.corner, dx, dy))
                CropDrag.Frame -> onFrame(current.moved(dx, dy))
                CropDrag.Nothing -> Unit
              }
            },
          )
        },
    ) {
      drawImage(
        image,
        dstOffset = IntOffset(picture.left.toInt(), picture.top.toInt()),
        dstSize = IntSize(picture.width.toInt(), picture.height.toInt()),
      )
      drawFrame(picture, frameRect(picture, frame))
    }
  }
}

private fun frameRect(picture: Rect, frame: CropFrame): Rect = Rect(
  picture.left + frame.left * picture.width,
  picture.top + frame.top * picture.height,
  picture.left + frame.right * picture.width,
  picture.top + frame.bottom * picture.height,
)

private fun dragAt(point: Offset, frame: Rect, reach: Float): CropDrag {
  val corners = listOf(
    CropCorner.TopLeft to frame.topLeft,
    CropCorner.TopRight to frame.topRight,
    CropCorner.BottomLeft to frame.bottomLeft,
    CropCorner.BottomRight to frame.bottomRight,
  )
  val (corner, at) = corners.minBy { (_, at) -> (at - point).getDistance() }
  return when {
    (at - point).getDistance() <= reach -> CropDrag.Corner(corner)
    frame.contains(point) -> CropDrag.Frame
    else -> CropDrag.Nothing
  }
}

private fun DrawScope.drawFrame(picture: Rect, frame: Rect) {
  val dim = Color.Black.copy(alpha = OUTSIDE_DIM)
  drawRect(dim, Offset(picture.left, picture.top), Size(picture.width, frame.top - picture.top))
  drawRect(dim, Offset(picture.left, frame.bottom), Size(picture.width, picture.bottom - frame.bottom))
  drawRect(dim, Offset(picture.left, frame.top), Size(frame.left - picture.left, frame.height))
  drawRect(dim, Offset(frame.right, frame.top), Size(picture.right - frame.right, frame.height))
  val line = 1.dp.toPx()
  drawRect(Color.White, frame.topLeft, frame.size, style = Stroke(line))
  // Thirds, to place what matters.
  val thirds = Color.White.copy(alpha = 0.35f)
  for (i in 1..2) {
    val x = frame.left + frame.width * i / 3
    val y = frame.top + frame.height * i / 3
    drawLine(thirds, Offset(x, frame.top), Offset(x, frame.bottom), line)
    drawLine(thirds, Offset(frame.left, y), Offset(frame.right, y), line)
  }
  val handle = HANDLE_LENGTH.toPx()
  val thick = 3.dp.toPx()
  for ((corner, xSign, ySign) in listOf(
    Triple(frame.topLeft, 1f, 1f),
    Triple(frame.topRight, -1f, 1f),
    Triple(frame.bottomLeft, 1f, -1f),
    Triple(frame.bottomRight, -1f, -1f),
  )) {
    drawLine(Color.White, corner, corner + Offset(handle * xSign, 0f), thick)
    drawLine(Color.White, corner, corner + Offset(0f, handle * ySign), thick)
  }
}
