package de.zeitgleich.teamcraft.ui.screens

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.camera.core.CameraSelector
import androidx.camera.core.ImageAnalysis
import androidx.camera.core.ImageProxy
import androidx.camera.core.Preview
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.camera.lifecycle.awaitInstance
import androidx.camera.view.PreviewView
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.core.content.ContextCompat
import androidx.lifecycle.LifecycleOwner
import androidx.lifecycle.compose.LocalLifecycleOwner
import com.google.zxing.BarcodeFormat
import com.google.zxing.BinaryBitmap
import com.google.zxing.DecodeHintType
import com.google.zxing.PlanarYUVLuminanceSource
import com.google.zxing.ReaderException
import com.google.zxing.common.HybridBinarizer
import com.google.zxing.qrcode.QRCodeReader
import de.zeitgleich.teamcraft.R
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors
import kotlinx.coroutines.awaitCancellation
import java.util.concurrent.atomic.AtomicBoolean

/** Width to height of the camera preview area. */
private const val PREVIEW_ASPECT_RATIO = 3f / 4f

/**
 * Camera preview that decodes QR codes on the device (zxing, no Google Play services) and reports
 * the first result once. Asks for the camera permission; if it's denied, shows a message so the
 * caller's manual entry stays the way in.
 */
@Composable
fun QrScanner(onScanned: (String) -> Unit, onCancel: () -> Unit, modifier: Modifier = Modifier) {
  val context = LocalContext.current
  var permission by remember { mutableStateOf(cameraPermission(context)) }
  val launcher = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { isGranted ->
    permission = if (isGranted) CameraPermission.Granted else CameraPermission.Denied
  }
  LaunchedEffect(Unit) {
    if (permission == CameraPermission.Unknown) launcher.launch(Manifest.permission.CAMERA)
  }
  Column(
    modifier = modifier.padding(16.dp),
    verticalArrangement = Arrangement.spacedBy(12.dp),
    horizontalAlignment = Alignment.CenterHorizontally,
  ) {
    when (permission) {
      CameraPermission.Granted -> {
        Text(stringResource(R.string.link_scan_hint), style = MaterialTheme.typography.bodyMedium)
        CameraPreview(onScanned)
      }
      CameraPermission.Denied -> Text(stringResource(R.string.link_camera_denied), color = MaterialTheme.colorScheme.error)
      CameraPermission.Unknown -> Unit
    }
    OutlinedButton(onClick = onCancel) { Text(stringResource(R.string.cancel)) }
  }
}

private enum class CameraPermission { Unknown, Granted, Denied }

private fun cameraPermission(context: Context): CameraPermission =
  if (ContextCompat.checkSelfPermission(context, Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED) {
    CameraPermission.Granted
  } else {
    CameraPermission.Unknown
  }

/** Binds preview and analysis to the lifecycle while shown; unbinds both when it leaves. */
@Composable
private fun CameraPreview(onScanned: (String) -> Unit) {
  val context = LocalContext.current
  val lifecycleOwner = LocalLifecycleOwner.current
  val currentOnScanned by rememberUpdatedState(onScanned)
  val previewView = remember { PreviewView(context) }
  LaunchedEffect(lifecycleOwner) {
    val analysisExecutor = Executors.newSingleThreadExecutor()
    val mainExecutor = ContextCompat.getMainExecutor(context)
    val hasReported = AtomicBoolean(false)
    val analyzer = QrAnalyzer { text ->
      if (hasReported.compareAndSet(false, true)) mainExecutor.execute { currentOnScanned(text) }
    }
    val provider = ProcessCameraProvider.awaitInstance(context)
    try {
      bindCamera(provider, lifecycleOwner, previewView, analysisExecutor, analyzer)
      awaitCancellation()
    } finally {
      provider.unbindAll()
      analysisExecutor.shutdown()
    }
  }
  AndroidView(factory = { previewView }, modifier = Modifier.fillMaxWidth().aspectRatio(PREVIEW_ASPECT_RATIO))
}

private fun bindCamera(
  provider: ProcessCameraProvider,
  lifecycleOwner: LifecycleOwner,
  previewView: PreviewView,
  analysisExecutor: ExecutorService,
  analyzer: ImageAnalysis.Analyzer,
) {
  val preview = Preview.Builder().build().also { it.setSurfaceProvider(previewView.surfaceProvider) }
  val analysis = ImageAnalysis.Builder()
    .setBackpressureStrategy(ImageAnalysis.STRATEGY_KEEP_ONLY_LATEST)
    .build()
    .also { it.setAnalyzer(analysisExecutor, analyzer) }
  provider.unbindAll()
  provider.bindToLifecycle(lifecycleOwner, CameraSelector.DEFAULT_BACK_CAMERA, preview, analysis)
}

/** Decodes QR codes from the luminance (Y) plane of each camera frame. */
private class QrAnalyzer(private val onResult: (String) -> Unit) : ImageAnalysis.Analyzer {
  private val reader = QRCodeReader()
  private val hints = mapOf(
    DecodeHintType.POSSIBLE_FORMATS to listOf(BarcodeFormat.QR_CODE),
    DecodeHintType.TRY_HARDER to true,
  )

  override fun analyze(image: ImageProxy) {
    try {
      decode(image)?.let(onResult)
    } finally {
      image.close()
    }
  }

  private fun decode(image: ImageProxy): String? {
    val plane = image.planes[0]
    val buffer = plane.buffer
    val data = ByteArray(buffer.remaining())
    buffer.get(data)
    val rowStride = plane.rowStride
    // The last row may be shorter than the stride; the source only reads `width` bytes per row.
    val dataHeight = (data.size + rowStride - 1) / rowStride
    if (dataHeight < image.height) return null
    val source = PlanarYUVLuminanceSource(data, rowStride, dataHeight, 0, 0, image.width, image.height, false)
    return try {
      reader.decode(BinaryBitmap(HybridBinarizer(source)), hints).text
    } catch (error: ReaderException) {
      // No (readable) QR code in this frame: the next frame tries again.
      null
    } finally {
      reader.reset()
    }
  }
}
