package de.zeitgleich.teamcraft.calls

import android.util.Size
import androidx.camera.core.CameraSelector
import androidx.camera.core.ImageAnalysis
import androidx.camera.core.ImageProxy
import androidx.camera.core.Preview
import androidx.camera.core.resolutionselector.ResolutionSelector
import androidx.camera.core.resolutionselector.ResolutionStrategy
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.camera.lifecycle.awaitInstance
import androidx.camera.view.PreviewView
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.viewinterop.AndroidView
import androidx.lifecycle.compose.LocalLifecycleOwner
import java.util.concurrent.Executors
import kotlinx.coroutines.awaitCancellation
import uniffi.teamcraft_ffi.CameraFrame

/** Capture size: enough for a phone's uplink, small enough to encode and convert per frame. */
private val CAPTURE_SIZE = Size(640, 480)
private const val NANOS_PER_MICRO = 1_000L
private const val MICROS_PER_SECOND = 1_000_000L

/**
 * The phone's camera in a call: a mirrored self-view and the frames for the engine, which rotates
 * them upright and encodes them. Runs while the call screen is visible.
 */
@Composable
fun CameraFeed(calls: CallController, isFrontCamera: Boolean, modifier: Modifier = Modifier) {
  val context = LocalContext.current
  val lifecycleOwner = LocalLifecycleOwner.current
  val previewView = remember { PreviewView(context).apply { scaleType = PreviewView.ScaleType.FILL_CENTER } }
  LaunchedEffect(lifecycleOwner, isFrontCamera) {
    val executor = Executors.newSingleThreadExecutor()
    val throttle = FrameThrottle(CallController.CAMERA_FRAMERATE)
    val provider = ProcessCameraProvider.awaitInstance(context)
    try {
      val preview = Preview.Builder().build().also { it.surfaceProvider = previewView.surfaceProvider }
      val analysis = ImageAnalysis.Builder()
        .setResolutionSelector(
          ResolutionSelector.Builder()
            .setResolutionStrategy(
              ResolutionStrategy(CAPTURE_SIZE, ResolutionStrategy.FALLBACK_RULE_CLOSEST_LOWER_THEN_HIGHER),
            )
            .build(),
        )
        .setBackpressureStrategy(ImageAnalysis.STRATEGY_KEEP_ONLY_LATEST)
        .setOutputImageFormat(ImageAnalysis.OUTPUT_IMAGE_FORMAT_YUV_420_888)
        .build()
      analysis.setAnalyzer(executor) { image ->
        image.use { if (throttle.isDue(it.imageInfo.timestamp)) send(calls, it) }
      }
      val selector = if (isFrontCamera) CameraSelector.DEFAULT_FRONT_CAMERA else CameraSelector.DEFAULT_BACK_CAMERA
      provider.unbindAll()
      provider.bindToLifecycle(lifecycleOwner, selector, preview, analysis)
      awaitCancellation()
    } finally {
      provider.unbindAll()
      executor.shutdown()
    }
  }
  AndroidView(factory = { previewView }, modifier = modifier)
}

/** Hands one camera image to the engine, planes and strides as they are. */
private fun send(calls: CallController, image: ImageProxy) {
  val (y, u, v) = image.planes
  val rotation = image.imageInfo.rotationDegrees
  val isTurned = rotation == 90 || rotation == 270
  val frame = CameraFrame(
    width = image.width.toUInt(),
    height = image.height.toUInt(),
    y = y.buffer.toByteArray(),
    u = u.buffer.toByteArray(),
    v = v.buffer.toByteArray(),
    yRowStride = y.rowStride.toUInt(),
    uvRowStride = u.rowStride.toUInt(),
    uvPixelStride = u.pixelStride.toUInt(),
    rotationDegrees = rotation.toUInt(),
    timestampUs = (image.imageInfo.timestamp / NANOS_PER_MICRO).toULong(),
  )
  val uprightWidth = if (isTurned) image.height else image.width
  val uprightHeight = if (isTurned) image.width else image.height
  calls.pushCameraFrame(frame, uprightWidth, uprightHeight)
}

private fun java.nio.ByteBuffer.toByteArray(): ByteArray {
  rewind()
  return ByteArray(remaining()).also { get(it) }
}

/** Lets frames through at the call's frame rate; the camera delivers more. */
private class FrameThrottle(framerate: Int) {
  private val intervalNanos = MICROS_PER_SECOND * NANOS_PER_MICRO / framerate
  private var lastNanos = Long.MIN_VALUE

  fun isDue(timestampNanos: Long): Boolean {
    if (lastNanos != Long.MIN_VALUE && timestampNanos - lastNanos < intervalNanos) return false
    lastNanos = timestampNanos
    return true
  }
}
