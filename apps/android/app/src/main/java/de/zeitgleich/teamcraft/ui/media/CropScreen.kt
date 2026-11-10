package de.zeitgleich.teamcraft.ui.media

import android.graphics.RectF
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectDragGestures
import androidx.compose.foundation.gestures.detectHorizontalDragGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.material3.Button
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.IconToggleButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.drawscope.DrawScope
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.ProgressBarRangeInfo
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.progressBarRangeInfo
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.setProgress
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.unit.dp
import kotlin.math.roundToInt
import de.zeitgleich.teamcraft.ui.screens.PageLoadingIndicator
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.ui.CropCorner
import de.zeitgleich.teamcraft.ui.CropFrame

/** How close to a corner a drag must start to move that corner. */
private val CORNER_REACH = 40.dp
private val FRAME_MARGIN = 28.dp
private val HANDLE_LENGTH = 22.dp
private const val OUTSIDE_DIM = 0.6f

/** The straightening dial: one tick per degree, this far apart. */
private val DEGREE_SPACING = 9.dp

/**
 * Crop as in the common messengers (#1358): a free frame with corner handles, a straightening dial,
 * a quarter turn, mirror and aspect lock, undo and redo, "Reset all"; Cancel keeps what was there.
 * Dark whatever the theme, like a photo editor.
 */
@Composable
fun CropScreen(uri: String, initial: PictureEdit, onCancel: () -> Unit, onDone: (CropEdit) -> Unit) {
  val maybePicture by rememberEditablePicture(uri)
  val history = rememberEditHistory(uri, initial.crop)
  val crop = history.value
  Column(Modifier.fillMaxSize().background(Color.Black).windowInsetsPadding(WindowInsets.safeDrawing)) {
    EditorTopBar(history, canReset = !crop.isNeutral, onReset = { history.set(CropEdit()) }, onCancel = onCancel)
    Box(Modifier.weight(1f).fillMaxWidth(), contentAlignment = Alignment.Center) {
      val picture = maybePicture
      if (picture == null) {
        PageLoadingIndicator(color = Color.White)
      } else {
        CropCanvas(picture, initial.copy(crop = crop), onStart = history::record, onCrop = { history.value = it })
      }
    }
    StraightenDial(
      degrees = crop.straightenDegrees,
      onStart = history::record,
      onDegrees = { history.value = history.value.copy(straightenDegrees = it) },
      modifier = Modifier.fillMaxWidth().padding(vertical = 8.dp),
    )
    Row(
      Modifier.fillMaxWidth().padding(horizontal = 8.dp, vertical = 8.dp),
      verticalAlignment = Alignment.CenterVertically,
    ) {
      IconButton(onClick = { history.set(crop.turnedLeft()) }) {
        Icon(painterResource(R.drawable.ic_rotate_left), contentDescription = stringResource(R.string.picture_rotate), tint = Color.White)
      }
      IconButton(onClick = { history.set(crop.mirrored()) }) {
        Icon(painterResource(R.drawable.ic_flip), contentDescription = stringResource(R.string.media_mirror), tint = Color.White)
      }
      IconToggleButton(checked = crop.isAspectLocked, onCheckedChange = { history.set(crop.copy(isAspectLocked = it)) }) {
        Icon(
          painterResource(if (crop.isAspectLocked) R.drawable.ic_lock else R.drawable.ic_lock_open),
          contentDescription = stringResource(R.string.media_aspect_lock),
          tint = if (crop.isAspectLocked) ACTIVE_TINT else Color.White,
        )
      }
      Box(Modifier.weight(1f))
      Button(onClick = { onDone(crop) }) { Text(stringResource(R.string.picture_edit_done)) }
    }
  }
}

/** Cancel, undo, redo and "Reset all": the top of both editors. */
@Composable
internal fun <T> EditorTopBar(history: EditHistory<T>, canReset: Boolean, onReset: () -> Unit, onCancel: () -> Unit) {
  Row(Modifier.fillMaxWidth().padding(horizontal = 4.dp, vertical = 4.dp), verticalAlignment = Alignment.CenterVertically) {
    TextButton(onClick = onCancel) { Text(stringResource(R.string.cancel), color = Color.White) }
    Box(Modifier.weight(1f))
    IconButton(onClick = history::undo, enabled = history.canUndo) {
      Icon(painterResource(R.drawable.ic_undo), contentDescription = stringResource(R.string.media_undo), tint = if (history.canUndo) Color.White else DISABLED)
    }
    IconButton(onClick = history::redo, enabled = history.canRedo) {
      Icon(painterResource(R.drawable.ic_redo), contentDescription = stringResource(R.string.media_redo), tint = if (history.canRedo) Color.White else DISABLED)
    }
    TextButton(onClick = onReset, enabled = canReset) {
      Text(stringResource(R.string.media_reset_all), color = if (canReset) Color.White else DISABLED)
    }
  }
}

/** What a drag that started at a point moves: a corner, the whole frame, or nothing. */
private sealed interface CropDrag {
  data class Corner(val corner: CropCorner) : CropDrag
  data object Frame : CropDrag
  data object Nothing : CropDrag
}

/** The turned and straightened picture, the area outside the frame dimmed, the frame with its corners. */
@Composable
private fun CropCanvas(picture: EditablePicture, edit: PictureEdit, onStart: () -> Unit, onCrop: (CropEdit) -> Unit) {
  val density = LocalDensity.current
  val description = stringResource(R.string.picture_crop_area)
  BoxWithConstraints(Modifier.fillMaxSize()) {
    val margin = with(density) { FRAME_MARGIN.toPx() }
    val width = constraints.maxWidth.toFloat()
    val height = constraints.maxHeight.toFloat()
    // The frame is drawn over the whole (turned) picture; the edits draw unframed.
    val fit = PictureFit.of(picture, edit.crop, isFramed = false, width, height, margin)
    val area = fit.bounds.toRect()
    val reach = with(density) { CORNER_REACH.toPx() }
    val current by rememberUpdatedState(edit.crop)
    EditedPictureCanvas(picture, edit, fit, Modifier.fillMaxSize())
    Canvas(
      Modifier
        .fillMaxSize()
        .semantics { contentDescription = description }
        .pointerInput(area) {
          var drag: CropDrag = CropDrag.Nothing
          detectDragGestures(
            onDragStart = { start ->
              drag = dragAt(start, frameRect(area, current.frame), reach)
              if (drag != CropDrag.Nothing) onStart()
            },
            onDrag = { change, amount ->
              change.consume()
              val dx = amount.x / area.width
              val dy = amount.y / area.height
              val frame = current.frame
              val moved = when (val active = drag) {
                is CropDrag.Corner -> if (current.isAspectLocked) {
                  frame.draggedLocked(active.corner, dx, dy, frame.width / frame.height)
                } else {
                  frame.dragged(active.corner, dx, dy)
                }
                CropDrag.Frame -> frame.moved(dx, dy)
                CropDrag.Nothing -> return@detectDragGestures
              }
              onCrop(current.copy(frame = moved))
            },
          )
        },
    ) {
      drawFrame(area, frameRect(area, edit.crop.frame))
    }
  }
}

private fun RectF.toRect(): Rect = Rect(left, top, right, bottom)

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

/**
 * The straightening dial: a ruler of degrees that slides under a fixed marker. Dragging turns the
 * picture up to [MAX_STRAIGHTEN_DEGREES] either way; a tap on the angle sets it back to level.
 */
@Composable
private fun StraightenDial(degrees: Float, onStart: () -> Unit, onDegrees: (Float) -> Unit, modifier: Modifier) {
  val density = LocalDensity.current
  val spacing = with(density) { DEGREE_SPACING.toPx() }
  val current by rememberUpdatedState(degrees)
  val label = stringResource(R.string.media_straighten)
  val shown = degrees.roundToInt()
  Column(modifier, horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(4.dp)) {
    Text(
      stringResource(R.string.media_degrees, shown),
      color = if (shown == 0) Color.White else ACTIVE_TINT,
      style = MaterialTheme.typography.labelLarge,
      modifier = Modifier.clickable(enabled = degrees != 0f) {
        onStart()
        onDegrees(0f)
      }.padding(horizontal = 12.dp, vertical = 2.dp),
    )
    Canvas(
      Modifier
        .fillMaxWidth()
        .height(32.dp)
        .semantics {
          contentDescription = label
          stateDescription = "$shown°"
          progressBarRangeInfo = ProgressBarRangeInfo(degrees, -MAX_STRAIGHTEN_DEGREES..MAX_STRAIGHTEN_DEGREES)
          setProgress { target ->
            onStart()
            onDegrees(target.roundToInt().toFloat().coerceIn(-MAX_STRAIGHTEN_DEGREES, MAX_STRAIGHTEN_DEGREES))
            true
          }
        }
        .pointerInput(Unit) {
          detectHorizontalDragGestures(onDragStart = { onStart() }) { change, amount ->
            change.consume()
            // Dragging the ruler left brings larger angles under the marker.
            onDegrees((current - amount / spacing).coerceIn(-MAX_STRAIGHTEN_DEGREES, MAX_STRAIGHTEN_DEGREES))
          }
        },
    ) {
      val middle = size.width / 2
      val first = (-MAX_STRAIGHTEN_DEGREES).toInt()
      for (degree in first..MAX_STRAIGHTEN_DEGREES.toInt()) {
        val x = middle + (degree - current) * spacing
        if (x < 0 || x > size.width) continue
        val isMajor = degree % 15 == 0
        val tick = if (isMajor) size.height * 0.6f else size.height * 0.35f
        val alpha = 1f - (kotlin.math.abs(x - middle) / middle).coerceIn(0f, 1f) * 0.8f
        drawLine(Color.White.copy(alpha = alpha), Offset(x, (size.height - tick) / 2), Offset(x, (size.height + tick) / 2), if (isMajor) 2.dp.toPx() else 1.dp.toPx())
      }
      drawLine(ACTIVE_TINT, Offset(middle, 0f), Offset(middle, size.height), 2.dp.toPx())
    }
  }
}

/** Active tools and changed values: the camera's yellow, readable on the dark editor. */
internal val ACTIVE_TINT = Color(0xFFFFD54F)
internal val DISABLED = Color.White.copy(alpha = 0.38f)
