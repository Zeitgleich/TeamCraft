package de.zeitgleich.teamcraft.ui.media

import android.Manifest
import android.annotation.SuppressLint
import android.content.Context
import android.content.pm.PackageManager
import android.graphics.Bitmap
import android.graphics.ImageFormat
import android.media.MediaMetadataRetriever
import android.net.Uri
import android.text.format.DateUtils
import android.view.HapticFeedbackConstants
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.camera.core.CameraSelector
import androidx.camera.core.ImageCapture
import androidx.camera.core.ImageCaptureException
import androidx.camera.core.ImageProxy
import androidx.camera.core.ZoomState
import androidx.camera.video.FileOutputOptions
import androidx.camera.video.Recording
import androidx.camera.video.VideoRecordEvent
import androidx.camera.view.CameraController
import androidx.camera.view.LifecycleCameraController
import androidx.camera.view.PreviewView
import androidx.camera.view.video.AudioConfig
import androidx.compose.animation.core.animateDpAsState
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.waitForUpOrCancellation
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.selection.selectable
import androidx.compose.material3.Button
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.onClick
import androidx.compose.ui.semantics.onLongClick
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.core.content.ContextCompat
import androidx.lifecycle.Observer
import androidx.lifecycle.compose.LocalLifecycleOwner
import java.io.File
import java.util.UUID
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.security.SealedMedia
import de.zeitgleich.teamcraft.ui.MediaAccess
import de.zeitgleich.teamcraft.ui.recentMediaThumbnail

/** Zoom steps offered as pills, where the lenses reach them. */
private val ZOOM_STEPS = listOf(0.5f, 1f, 2f, 5f)

/** While recording, the elapsed time updates this often. */
private const val RECORDING_TICK_MS = 500L

/** What the shutter does on a tap. */
private enum class CameraMode { Photo, Video }

/**
 * The app's own camera, as in the common messengers (#1358): full screen, close top left, flash top
 * right, zoom pills above the shutter, the newest gallery picture bottom left, flip on the right,
 * Photo / Video below. A tap takes a photo, holding records a video until released; in Video mode
 * a tap starts and stops. Photos and videos stay in the app's own folder until they are sent,
 * sealed ([SealedCaptures]): a photo is taken into memory and sealed before it is written, a video
 * is sealed as soon as its recording ends.
 */
@Composable
fun CameraScreen(
  capturesDirectory: File,
  selectedCount: Int,
  onCaptured: (MediaDraftItem) -> Unit,
  onGallery: () -> Unit,
  onReview: () -> Unit,
  onClose: () -> Unit,
) {
  val context = LocalContext.current
  var hasCamera by remember { mutableStateOf(isGranted(context, Manifest.permission.CAMERA)) }
  var isAsked by rememberSaveable { mutableStateOf(false) }
  val ask = rememberLauncherForActivityResult(ActivityResultContracts.RequestMultiplePermissions()) {
    hasCamera = isGranted(context, Manifest.permission.CAMERA)
    isAsked = true
  }
  LaunchedEffect(Unit) {
    // The microphone is asked along: a video without it is recorded silent.
    if (!hasCamera && !isAsked) ask.launch(arrayOf(Manifest.permission.CAMERA, Manifest.permission.RECORD_AUDIO))
  }
  Box(Modifier.fillMaxSize().background(Color.Black)) {
    if (hasCamera) {
      CameraContent(capturesDirectory, selectedCount, onCaptured, onGallery, onReview, onClose)
    } else {
      Column(
        Modifier.align(Alignment.Center).padding(32.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(16.dp),
      ) {
        Text(stringResource(R.string.media_camera_denied), color = Color.White, textAlign = TextAlign.Center)
        Button(onClick = onGallery) { Text(stringResource(R.string.media_open_gallery)) }
      }
      IconButton(onClick = onClose, modifier = Modifier.windowInsetsPadding(WindowInsets.safeDrawing).padding(8.dp)) {
        Icon(painterResource(R.drawable.ic_close), contentDescription = stringResource(R.string.media_close), tint = Color.White)
      }
    }
  }
}

@Composable
private fun CameraContent(
  capturesDirectory: File,
  selectedCount: Int,
  onCaptured: (MediaDraftItem) -> Unit,
  onGallery: () -> Unit,
  onReview: () -> Unit,
  onClose: () -> Unit,
) {
  val context = LocalContext.current
  val view = LocalView.current
  val lifecycleOwner = LocalLifecycleOwner.current
  val controller = remember {
    LifecycleCameraController(context).apply {
      setEnabledUseCases(CameraController.IMAGE_CAPTURE or CameraController.VIDEO_CAPTURE)
      imageCaptureMode = ImageCapture.CAPTURE_MODE_MINIMIZE_LATENCY
    }
  }
  DisposableEffect(lifecycleOwner) {
    controller.bindToLifecycle(lifecycleOwner)
    onDispose { controller.unbind() }
  }
  var mode by rememberSaveable { mutableStateOf(CameraMode.Photo) }
  var flash by rememberSaveable { mutableStateOf(FlashChoice.Off) }
  var isFront by rememberSaveable { mutableStateOf(false) }
  var maybeRecording by remember { mutableStateOf<Recording?>(null) }
  var recordingStartedAt by remember { mutableLongStateOf(0L) }
  var isBusy by remember { mutableStateOf(false) }
  val currentOnCaptured by rememberUpdatedState(onCaptured)
  val scope = rememberCoroutineScope()
  var zoom by remember { mutableStateOf<ZoomState?>(null) }
  DisposableEffect(controller, lifecycleOwner) {
    val observer = Observer<ZoomState> { zoom = it }
    controller.zoomState.observe(lifecycleOwner, observer)
    onDispose { controller.zoomState.removeObserver(observer) }
  }

  LaunchedEffect(isFront) {
    controller.cameraSelector = if (isFront) CameraSelector.DEFAULT_FRONT_CAMERA else CameraSelector.DEFAULT_BACK_CAMERA
  }
  LaunchedEffect(flash, mode, maybeRecording) {
    controller.imageCaptureFlashMode = flash.captureMode
    // Video has no flash, only the light: on while recording when the flash is on.
    controller.enableTorch(flash == FlashChoice.On && (mode == CameraMode.Video || maybeRecording != null))
  }

  fun takePhoto() {
    if (isBusy) return
    isBusy = true
    val file = File(capturesDirectory, "photo-${System.currentTimeMillis()}.jpg${SealedCaptures.SUFFIX}")
    // The front camera's photo is mirrored, as its preview shows it (CameraX does so for files).
    val isMirrored = isFront
    view.performHapticFeedback(HapticFeedbackConstants.VIRTUAL_KEY)
    controller.takePicture(
      ContextCompat.getMainExecutor(context),
      object : ImageCapture.OnImageCapturedCallback() {
        override fun onCaptureSuccess(image: ImageProxy) {
          // Copied out before the image goes back to the camera; it never touches the disk plain.
          val maybeCaptured = runCatching { image.use(::capturedImage) }.getOrNull()
          scope.launch {
            val isSealed = maybeCaptured != null && withContext(Dispatchers.IO) {
              runCatching { SealedMedia.sealTo(file, SealedCaptures.key(context), maybeCaptured.jpeg(isMirrored)) }.isSuccess
            }
            isBusy = false
            if (isSealed) {
              currentOnCaptured(MediaDraftItem(id = UUID.randomUUID().toString(), uri = Uri.fromFile(file).toString(), isVideo = false, isCaptured = true))
            } else {
              file.delete()
            }
          }
        }

        override fun onError(exception: ImageCaptureException) {
          isBusy = false
        }
      },
    )
  }

  // The microphone is used only when granted (AudioConfig below), otherwise the video is silent.
  @SuppressLint("MissingPermission")
  fun startRecording() {
    if (maybeRecording != null) return
    val file = File(capturesDirectory, "video-${System.currentTimeMillis()}.mp4")
    val audio = AudioConfig.create(isGranted(context, Manifest.permission.RECORD_AUDIO))
    view.performHapticFeedback(HapticFeedbackConstants.LONG_PRESS)
    recordingStartedAt = System.currentTimeMillis()
    maybeRecording = runCatching {
      controller.startRecording(FileOutputOptions.Builder(file).build(), audio, ContextCompat.getMainExecutor(context)) { event ->
        if (event is VideoRecordEvent.Finalize) {
          maybeRecording = null
          if ((event.hasError() && event.error != VideoRecordEvent.Finalize.ERROR_SOURCE_INACTIVE) || !file.exists() || file.length() == 0L) {
            file.delete()
          } else {
            // The recorder writes a seekable file, so the video is sealed once it is complete.
            isBusy = true
            scope.launch {
              val (durationMs, kept) = withContext(Dispatchers.IO) { videoDuration(file) to sealRecording(context, file) }
              isBusy = false
              currentOnCaptured(MediaDraftItem(id = UUID.randomUUID().toString(), uri = Uri.fromFile(kept).toString(), isVideo = true, durationMs = durationMs, isCaptured = true))
            }
          }
        }
      }
    }.getOrNull()
  }

  fun stopRecording() {
    maybeRecording?.stop()
  }

  Box(Modifier.fillMaxSize()) {
    AndroidView(
      factory = { viewContext ->
        PreviewView(viewContext).apply {
          scaleType = PreviewView.ScaleType.FILL_CENTER
          this.controller = controller
        }
      },
      modifier = Modifier.fillMaxSize(),
    )
    Column(Modifier.fillMaxSize().windowInsetsPadding(WindowInsets.safeDrawing)) {
      Row(Modifier.fillMaxWidth().padding(8.dp), verticalAlignment = Alignment.CenterVertically) {
        IconButton(onClick = onClose, modifier = Modifier.background(SCRIM, CircleShape)) {
          Icon(painterResource(R.drawable.ic_close), contentDescription = stringResource(R.string.media_close), tint = Color.White)
        }
        Spacer(Modifier.weight(1f))
        if (maybeRecording != null) RecordingTime(recordingStartedAt)
        Spacer(Modifier.weight(1f))
        IconButton(onClick = { flash = flash.next() }, modifier = Modifier.background(SCRIM, CircleShape)) {
          Icon(painterResource(flash.icon), contentDescription = stringResource(flash.label), tint = Color.White)
        }
      }
      Spacer(Modifier.weight(1f))
      zoom?.let { state ->
        ZoomPills(
          steps = ZOOM_STEPS.filter { it in state.minZoomRatio - 0.05f..state.maxZoomRatio + 0.05f },
          current = state.zoomRatio,
          onZoom = { controller.setZoomRatio(it) },
          modifier = Modifier.align(Alignment.CenterHorizontally).padding(bottom = 16.dp),
        )
      }
      Row(
        Modifier.fillMaxWidth().padding(horizontal = 24.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.SpaceBetween,
      ) {
        NewestPicture(onGallery)
        Shutter(
          mode = mode,
          isRecording = maybeRecording != null,
          onTap = {
            when {
              maybeRecording != null -> stopRecording()
              mode == CameraMode.Photo -> takePhoto()
              else -> startRecording()
            }
          },
          onHoldStart = ::startRecording,
          onHoldEnd = ::stopRecording,
        )
        IconButton(
          onClick = { isFront = !isFront },
          enabled = maybeRecording == null,
          modifier = Modifier.size(52.dp).background(SCRIM, CircleShape),
        ) {
          Icon(painterResource(R.drawable.ic_cameraswitch), contentDescription = stringResource(R.string.media_camera_flip), tint = Color.White)
        }
      }
      Box(Modifier.fillMaxWidth().padding(vertical = 16.dp, horizontal = 24.dp)) {
        ModeSwitch(mode, isEnabled = maybeRecording == null, onMode = { mode = it }, modifier = Modifier.align(Alignment.Center))
        // Back to what is picked already: without it, the camera reached from the review could
        // only take another one or discard them all.
        if (selectedCount > 0 && maybeRecording == null) {
          SelectedButton(selectedCount, onReview, Modifier.align(Alignment.CenterEnd))
        }
      }
    }
  }
}

/** How many are picked, and the way back to them. */
@Composable
private fun SelectedButton(count: Int, onClick: () -> Unit, modifier: Modifier) {
  val label = pluralStringResource(R.plurals.media_next, count, count)
  Row(
    modifier
      .clip(RoundedCornerShape(50))
      .background(Color.White)
      .clickable(onClickLabel = label, onClick = onClick)
      .semantics(mergeDescendants = true) { contentDescription = label }
      .padding(horizontal = 12.dp, vertical = 8.dp),
    verticalAlignment = Alignment.CenterVertically,
    horizontalArrangement = Arrangement.spacedBy(4.dp),
  ) {
    Text(count.toString(), color = Color.Black, style = MaterialTheme.typography.labelLarge)
    Icon(painterResource(R.drawable.ic_arrow_forward), contentDescription = null, tint = Color.Black, modifier = Modifier.size(18.dp))
  }
}

/** Flash for photos: off, automatic, on; for video "on" is the light while recording. */
private enum class FlashChoice(val captureMode: Int, val icon: Int, val label: Int) {
  Off(ImageCapture.FLASH_MODE_OFF, R.drawable.ic_flash_off, R.string.media_flash_off),
  Auto(ImageCapture.FLASH_MODE_AUTO, R.drawable.ic_flash_auto, R.string.media_flash_auto),
  On(ImageCapture.FLASH_MODE_ON, R.drawable.ic_flash_on, R.string.media_flash_on);

  fun next(): FlashChoice = entries[(ordinal + 1) % entries.size]
}

@Composable
private fun RecordingTime(startedAt: Long) {
  val elapsed by produceState(0L, startedAt) {
    while (true) {
      value = System.currentTimeMillis() - startedAt
      delay(RECORDING_TICK_MS)
    }
  }
  Row(
    Modifier.background(RECORDING_RED, RoundedCornerShape(50)).padding(horizontal = 12.dp, vertical = 4.dp),
    verticalAlignment = Alignment.CenterVertically,
  ) {
    Box(Modifier.size(8.dp).background(Color.White, CircleShape))
    Text(DateUtils.formatElapsedTime(elapsed / 1000), color = Color.White, modifier = Modifier.padding(start = 6.dp))
  }
}

@Composable
private fun ZoomPills(steps: List<Float>, current: Float, onZoom: (Float) -> Unit, modifier: Modifier) {
  if (steps.size < 2) return
  // The pill nearest the current zoom is lit, and shows the exact zoom while pinching.
  val active = steps.minBy { kotlin.math.abs(it - current) }
  Row(modifier.background(SCRIM, RoundedCornerShape(50)).padding(4.dp), horizontalArrangement = Arrangement.spacedBy(4.dp)) {
    for (step in steps) {
      val isActive = step == active
      val label = if (isActive) zoomLabel(current) + "×" else zoomLabel(step)
      val description = stringResource(R.string.media_zoom, zoomLabel(step))
      Box(
        Modifier
          .size(if (isActive) 40.dp else 34.dp)
          .clip(CircleShape)
          .background(if (isActive) Color.White.copy(alpha = 0.25f) else Color.Transparent)
          .selectable(selected = isActive, role = Role.RadioButton, onClick = { onZoom(step) })
          .semantics { contentDescription = description },
        contentAlignment = Alignment.Center,
      ) {
        Text(label, color = if (isActive) ACCENT_YELLOW else Color.White, style = androidx.compose.material3.MaterialTheme.typography.labelMedium)
      }
    }
  }
}

private fun zoomLabel(ratio: Float): String {
  val rounded = Math.round(ratio * 10) / 10f
  return if (rounded % 1f == 0f) rounded.toInt().toString() else rounded.toString()
}

/** The newest gallery picture; a tap opens the gallery. Without access: the gallery symbol. */
@Composable
private fun NewestPicture(onGallery: () -> Unit) {
  val context = LocalContext.current
  val sidePx = with(LocalDensity.current) { 52.dp.roundToPx() }
  val maybeThumbnail by produceState<ImageBitmap?>(null) {
    if (MediaAccess.of(context) == MediaAccess.None) return@produceState
    val newest = MediaLibrary.newest(context) ?: return@produceState
    value = recentMediaThumbnail(context, newest, sidePx)?.asImageBitmap()
  }
  val description = stringResource(R.string.media_open_gallery)
  Box(
    Modifier
      .size(52.dp)
      .clip(RoundedCornerShape(12.dp))
      .background(SCRIM)
      .border(2.dp, Color.White, RoundedCornerShape(12.dp))
      .clickable(onClick = onGallery)
      .semantics { contentDescription = description },
    contentAlignment = Alignment.Center,
  ) {
    val thumbnail = maybeThumbnail
    if (thumbnail != null) {
      Image(thumbnail, contentDescription = null, contentScale = ContentScale.Crop, modifier = Modifier.matchParentSize())
    } else {
      Icon(painterResource(R.drawable.ic_photo_library), contentDescription = null, tint = Color.White)
    }
  }
}

/**
 * The shutter: a white ring. A tap takes a photo (or starts and stops a video in Video mode); held,
 * it records until released, and turns red with a square while recording.
 */
@Composable
private fun Shutter(mode: CameraMode, isRecording: Boolean, onTap: () -> Unit, onHoldStart: () -> Unit, onHoldEnd: () -> Unit) {
  val tap by rememberUpdatedState(onTap)
  val holdStart by rememberUpdatedState(onHoldStart)
  val holdEnd by rememberUpdatedState(onHoldEnd)
  val inner by animateDpAsState(if (isRecording) 30.dp else 62.dp, label = "shutter")
  val description = stringResource(if (mode == CameraMode.Photo) R.string.media_shutter_photo else R.string.media_shutter_video)
  val holdLabel = stringResource(R.string.media_shutter_hold)
  Box(
    Modifier
      .size(80.dp)
      .border(4.dp, Color.White, CircleShape)
      .semantics {
        contentDescription = description
        onClick { tap(); true }
        onLongClick(label = holdLabel) { holdStart(); true }
      }
      .pointerInput(Unit) {
        awaitEachGesture {
          awaitFirstDown()
          var isReleased = false
          withTimeoutOrNull(viewConfiguration.longPressTimeoutMillis) {
            waitForUpOrCancellation()
            isReleased = true
          }
          if (isReleased) {
            tap()
          } else {
            holdStart()
            waitForUpOrCancellation()
            holdEnd()
          }
        }
      },
    contentAlignment = Alignment.Center,
  ) {
    val color = if (isRecording || mode == CameraMode.Video) RECORDING_RED else Color.White
    Box(Modifier.size(inner).background(color, if (isRecording) RoundedCornerShape(8.dp) else CircleShape))
  }
}

@Composable
private fun ModeSwitch(mode: CameraMode, isEnabled: Boolean, onMode: (CameraMode) -> Unit, modifier: Modifier) {
  Row(modifier.background(SCRIM, RoundedCornerShape(50)).padding(4.dp)) {
    for ((choice, label) in listOf(CameraMode.Photo to R.string.media_mode_photo, CameraMode.Video to R.string.media_mode_video)) {
      val isSelected = mode == choice
      Text(
        stringResource(label),
        color = if (isSelected) Color.Black else Color.White,
        style = androidx.compose.material3.MaterialTheme.typography.labelLarge,
        modifier = Modifier
          .clip(RoundedCornerShape(50))
          .background(if (isSelected) Color.White else Color.Transparent)
          .selectable(selected = isSelected, enabled = isEnabled, role = Role.Tab, onClick = { onMode(choice) })
          .padding(horizontal = 16.dp, vertical = 6.dp),
      )
    }
  }
}

private fun isGranted(context: Context, permission: String): Boolean =
  ContextCompat.checkSelfPermission(context, permission) == PackageManager.PERMISSION_GRANTED

/** A photo as it came from the camera: still to be turned and mirrored. */
private class CapturedImage(private val maybeJpeg: ByteArray?, private val maybeBitmap: Bitmap?, private val rotationDegrees: Int) {
  /** The JPEG to seal, told how to show it ([CapturedJpeg]). */
  fun jpeg(isMirrored: Boolean): ByteArray {
    val plain = maybeJpeg ?: java.io.ByteArrayOutputStream().use { out ->
      requireNotNull(maybeBitmap).compress(Bitmap.CompressFormat.JPEG, CAPTURE_JPEG_QUALITY, out)
      out.toByteArray()
    }
    return CapturedJpeg.withOrientation(plain, rotationDegrees, isMirrored)
  }
}

private const val CAPTURE_JPEG_QUALITY = 95

private fun capturedImage(image: ImageProxy): CapturedImage {
  val rotation = image.imageInfo.rotationDegrees
  if (image.format != ImageFormat.JPEG) return CapturedImage(null, image.toBitmap(), rotation)
  val buffer = image.planes[0].buffer.duplicate().apply { rewind() }
  return CapturedImage(ByteArray(buffer.remaining()).also(buffer::get), null, rotation)
}

/**
 * The recording sealed beside and the plain file removed; the plain file itself when it can't be
 * sealed (no key): it is sealed again when the draft is next opened, or goes with the flow.
 */
private fun sealRecording(context: Context, plain: File): File {
  val target = File(plain.parentFile, "${plain.name}${SealedCaptures.SUFFIX}")
  val isSealed = runCatching { SealedMedia.sealTo(target, SealedCaptures.key(context), plain) }.isSuccess
  if (!isSealed) return plain
  plain.delete()
  return target
}

/** A recorded video's length; 0 when the file can't be read (it then shows no length). */
internal fun videoDuration(file: File): Long = runCatching {
  MediaMetadataRetriever().run {
    try {
      setDataSource(file.absolutePath)
      extractMetadata(MediaMetadataRetriever.METADATA_KEY_DURATION)?.toLongOrNull() ?: 0L
    } finally {
      release()
    }
  }
}.getOrDefault(0L)

private val SCRIM = Color.Black.copy(alpha = 0.35f)
private val RECORDING_RED = Color(0xFFE53935)

/** The camera's zoom highlight: yellow reads on any scene, as in the system cameras. */
private val ACCENT_YELLOW = Color(0xFFFFD54F)
