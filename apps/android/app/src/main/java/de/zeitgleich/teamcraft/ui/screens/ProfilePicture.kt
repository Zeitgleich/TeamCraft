package de.zeitgleich.teamcraft.ui.screens

import android.graphics.Bitmap
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Edit
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Icon
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.foundation.gestures.detectTransformGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Slider
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.graphics.ClipOp
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.FilterQuality
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.drawscope.clipPath
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.OwnProfile
import de.zeitgleich.teamcraft.ui.AvatarCrop
import de.zeitgleich.teamcraft.ui.CropSquare
import de.zeitgleich.teamcraft.ui.MainViewModel
import de.zeitgleich.teamcraft.ui.media.PictureRenderer
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.io.ByteArrayOutputStream
import kotlin.math.min
import kotlin.math.roundToInt

/** The picture is decoded at most this large for the crop screen. */
private const val MAX_EDIT_SIDE_PX = 2048

/** The side of the square sent to the server; it re-encodes and limits it again. */
private const val UPLOAD_SIDE_PX = 512

/** Space between the circle and the edges of the crop screen. */
private val CIRCLE_MARGIN = 24.dp

/**
 * The top of Settings: the member's picture and, beside it, [details] (name, presence, status).
 * Tapping the picture opens a small menu with the rare profile actions (issue #2071): change or
 * remove the picture (photo picker, then move and zoom under a circle), change the name or go back
 * to the sign-in name (issue #408). Nothing takes room until it is used: the name field shows only
 * while editing, errors only when there is one. While the workspace shows the identity provider's
 * picture without overrides, the menu says where the picture comes from instead of offering it.
 */
@Composable
internal fun ProfileHeader(
  viewModel: MainViewModel,
  userId: String,
  displayName: String,
  details: @Composable ColumnScope.() -> Unit,
) {
  val context = LocalContext.current
  val scope = rememberCoroutineScope()
  val changes by viewModel.changeCount.collectAsState()
  var reloads by remember { mutableIntStateOf(0) }
  var maybeProfile by remember { mutableStateOf<OwnProfile?>(null) }
  LaunchedEffect(changes, reloads) { maybeProfile = viewModel.readChat { ownProfile() } ?: maybeProfile }
  var isMenuOpen by remember { mutableStateOf(false) }
  var isEditingName by rememberSaveable { mutableStateOf(false) }
  var maybePicked by remember { mutableStateOf<Bitmap?>(null) }
  var isSaving by remember { mutableStateOf(false) }
  var maybeFailure by remember { mutableStateOf<Int?>(null) }
  val picker = rememberLauncherForActivityResult(ActivityResultContracts.PickVisualMedia()) { maybeUri ->
    val uri = maybeUri ?: return@rememberLauncherForActivityResult
    scope.launch {
      val decoded = withContext(Dispatchers.IO) { PictureRenderer.decodeUpright(context, uri, MAX_EDIT_SIDE_PX) }
      if (decoded == null) maybeFailure = R.string.profile_picture_unreadable else maybePicked = decoded
    }
  }
  fun act(action: suspend () -> Int?) {
    isMenuOpen = false
    isSaving = true
    maybeFailure = null
    scope.launch {
      maybeFailure = action()
      reloads++
      isSaving = false
    }
  }
  val profile = maybeProfile
  val menuLabel = stringResource(R.string.profile_actions)
  Row(verticalAlignment = Alignment.CenterVertically) {
    Box {
      Box(
        Modifier
          .clip(CircleShape)
          .clickable(enabled = profile != null && !isSaving, onClickLabel = menuLabel) { isMenuOpen = true },
      ) {
        AccountAvatar(userId, displayName, size = 56.dp)
      }
      if (profile != null) {
        Icon(
          Icons.Filled.Edit,
          contentDescription = null,
          tint = MaterialTheme.colorScheme.onSurface,
          modifier = Modifier
            .align(Alignment.BottomEnd)
            .size(20.dp)
            .background(MaterialTheme.colorScheme.surfaceContainerHighest, CircleShape)
            .padding(3.dp),
        )
      }
      if (profile != null) {
        DropdownMenu(expanded = isMenuOpen, onDismissRequest = { isMenuOpen = false }) {
          if (profile.canChangeAvatar) {
            DropdownMenuItem(
              text = { Text(stringResource(R.string.profile_picture_change)) },
              onClick = {
                isMenuOpen = false
                maybeFailure = null
                picker.launch(PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageOnly))
              },
            )
            if (profile.hasAvatar) {
              DropdownMenuItem(
                text = { Text(stringResource(R.string.profile_picture_remove)) },
                onClick = { act { viewModel.tryRemoveOwnAvatar(userId) } },
              )
            }
          } else {
            DropdownMenuItem(
              text = { Text(stringResource(R.string.profile_picture_from_provider), style = MaterialTheme.typography.bodySmall) },
              onClick = {},
              enabled = false,
            )
          }
          if (profile.canOverrideDisplayName) {
            DropdownMenuItem(
              text = { Text(stringResource(R.string.profile_name_change)) },
              onClick = {
                isMenuOpen = false
                isEditingName = true
              },
            )
            if (profile.maybeDisplayNameOverride != null) {
              DropdownMenuItem(
                text = { Text(stringResource(R.string.profile_name_reset)) },
                onClick = { act { viewModel.trySetDisplayName(profile, null) } },
              )
            }
          }
        }
      }
    }
    Column(Modifier.padding(start = 16.dp).weight(1f), content = details)
  }
  if (maybePicked == null) FailureText(maybeFailure)
  if (isEditingName && profile != null) {
    DisplayNameEditor(viewModel, profile, onClose = {
      isEditingName = false
      reloads++
    })
  }
  val picked = maybePicked
  if (picked != null) {
    AvatarCropScreen(
      picture = picked,
      isSaving = isSaving,
      maybeFailure = maybeFailure,
      onCancel = {
        maybePicked = null
        maybeFailure = null
      },
      onUse = { square ->
        isSaving = true
        maybeFailure = null
        scope.launch {
          val png = withContext(Dispatchers.Default) { croppedPng(picked, square) }
          val failure = viewModel.trySetOwnAvatar(userId, png)
          maybeFailure = failure
          if (failure == null) {
            maybePicked = null
            reloads++
          }
          isSaving = false
        }
      },
    )
  }
}

/**
 * Full screen: the picture moves (drag) and zooms (pinch, or the slider for those who can't pinch)
 * under a fixed circle; outside the circle is dimmed. Cancel and Use at the top, like the system's
 * photo editors.
 */
@Composable
private fun AvatarCropScreen(
  picture: Bitmap,
  isSaving: Boolean,
  maybeFailure: Int?,
  onCancel: () -> Unit,
  onUse: (CropSquare) -> Unit,
) {
  val image = remember(picture) { picture.asImageBitmap() }
  var crop by remember(picture) { mutableStateOf(AvatarCrop(picture.width, picture.height)) }
  val area = stringResource(R.string.profile_picture_crop_area)
  val zoomLabel = stringResource(R.string.profile_picture_zoom)
  Dialog(
    onDismissRequest = { if (!isSaving) onCancel() },
    // Edge to edge: the black screen also covers the system bars' areas.
    properties = DialogProperties(usePlatformDefaultWidth = false, decorFitsSystemWindows = false),
  ) {
    Surface(Modifier.fillMaxSize(), color = Color.Black) {
      Column(
        Modifier.fillMaxSize().windowInsetsPadding(WindowInsets.safeDrawing).padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(16.dp),
      ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
          TextButton(onClick = onCancel, enabled = !isSaving) { Text(stringResource(R.string.cancel), color = Color.White) }
          Text(
            stringResource(R.string.profile_picture_crop_title),
            modifier = Modifier.weight(1f),
            textAlign = TextAlign.Center,
            color = Color.White,
            style = MaterialTheme.typography.titleMedium,
          )
          Button(onClick = { onUse(crop.square()) }, enabled = !isSaving) { Text(stringResource(R.string.profile_picture_use)) }
        }
        Box(Modifier.weight(1f).fillMaxWidth(), contentAlignment = Alignment.Center) {
          Canvas(
            Modifier
              .fillMaxSize()
              // A zoomed picture stays in its area, not over the buttons and the slider.
              .clipToBounds()
              .semantics { contentDescription = area }
              .pointerInput(picture) {
                detectTransformGestures { _, pan, zoom, _ ->
                  val circleSide = min(size.width, size.height) - 2 * CIRCLE_MARGIN.toPx()
                  crop = crop.withZoom(crop.zoom * zoom).dragged(pan.x, pan.y, circleSide)
                }
              },
          ) {
            val circleSide = min(size.width, size.height) - 2 * CIRCLE_MARGIN.toPx()
            if (circleSide <= 0f) return@Canvas
            val scale = circleSide / crop.side
            val circleTopLeft = Offset(center.x - circleSide / 2, center.y - circleSide / 2)
            drawImage(
              image,
              dstOffset = IntOffset(
                (circleTopLeft.x - crop.left * scale).roundToInt(),
                (circleTopLeft.y - crop.top * scale).roundToInt(),
              ),
              dstSize = IntSize((picture.width * scale).roundToInt(), (picture.height * scale).roundToInt()),
              filterQuality = FilterQuality.Medium,
            )
            val circle = Path().apply { addOval(Rect(center, circleSide / 2)) }
            clipPath(circle, clipOp = ClipOp.Difference) { drawRect(Color.Black.copy(alpha = 0.6f)) }
            drawCircle(Color.White.copy(alpha = 0.8f), radius = circleSide / 2, center = center, style = Stroke(1.dp.toPx()))
          }
        }
        if (crop.maxZoom > 1.01f) {
          Slider(
            value = crop.zoom,
            onValueChange = { crop = crop.withZoom(it) },
            valueRange = 1f..crop.maxZoom,
            enabled = !isSaving,
            modifier = Modifier.fillMaxWidth().semantics { contentDescription = zoomLabel },
          )
        }
        FailureText(maybeFailure)
      }
    }
  }
}

@Composable
private fun FailureText(maybeFailure: Int?) {
  maybeFailure?.let {
    Text(
      stringResource(it),
      style = MaterialTheme.typography.bodySmall,
      color = MaterialTheme.colorScheme.error,
      modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
    )
  }
}

/** The square of [picture] as a PNG at most [UPLOAD_SIDE_PX] wide; call off the main thread. */
private fun croppedPng(picture: Bitmap, square: CropSquare): ByteArray {
  val cut = Bitmap.createBitmap(picture, square.left, square.top, square.side, square.side)
  val side = min(square.side, UPLOAD_SIDE_PX)
  val scaled = if (side == square.side) cut else Bitmap.createScaledBitmap(cut, side, side, true)
  return ByteArrayOutputStream().use { out ->
    scaled.compress(Bitmap.CompressFormat.PNG, 100, out)
    out.toByteArray()
  }
}
