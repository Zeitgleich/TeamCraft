package de.zeitgleich.teamcraft.ui.screens

import android.content.Context
import kotlinx.coroutines.launch
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.material3.TextButton
import androidx.annotation.RequiresApi
import android.widget.Toast
import android.provider.MediaStore
import android.os.Environment
import android.os.Build
import android.content.ContentValues
import android.content.Intent
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.Matrix
import android.media.ExifInterface
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.gestures.rememberTransformableState
import androidx.compose.foundation.gestures.transformable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Share
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.core.content.FileProvider
import java.io.File
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import de.zeitgleich.teamcraft.R

/** The folder under Pictures that saved images go to. */
private const val GALLERY_FOLDER = "TeamCraft"

/** Zoom limits of the viewer; double tap toggles between 1× and this. */
private const val MAX_ZOOM = 5f
private const val DOUBLE_TAP_ZOOM = 2.5f

/** A decrypted image to look at full screen. */
data class ViewedImage(val path: String, val fileName: String, val mediaType: String)

/**
 * Full-screen image: pinch to zoom, drag to pan, double tap to zoom in and out; share and open in
 * another app from the top bar. The image is decoded at screen size, not at its full resolution.
 */
@Composable
fun ImageViewer(image: ViewedImage, onDismiss: () -> Unit) {
  val context = LocalContext.current
  val scope = rememberCoroutineScope()
  val configuration = LocalConfiguration.current
  val density = LocalDensity.current
  val maxSidePx = with(density) { maxOf(configuration.screenWidthDp, configuration.screenHeightDp).dp.roundToPx() }
  var maybeBitmap by remember(image.path) { mutableStateOf<ImageBitmap?>(null) }
  var hasFailed by remember(image.path) { mutableStateOf(false) }
  LaunchedEffect(image.path) {
    val decoded = withContext(Dispatchers.IO) { decodeSampled(image.path, maxSidePx) }
    maybeBitmap = decoded?.asImageBitmap()
    hasFailed = decoded == null
  }
  var scale by remember { mutableFloatStateOf(1f) }
  var offset by remember { mutableStateOf(Offset.Zero) }
  val transform = rememberTransformableState { _: Offset, zoom: Float, pan: Offset, _: Float ->
    scale = (scale * zoom).coerceIn(1f, MAX_ZOOM)
    offset = if (scale == 1f) Offset.Zero else offset + pan
  }
  Dialog(
    onDismissRequest = onDismiss,
    properties = DialogProperties(usePlatformDefaultWidth = false, decorFitsSystemWindows = false),
  ) {
    Box(Modifier.fillMaxSize().background(Color.Black)) {
      val bitmap = maybeBitmap
      when {
        bitmap != null -> Image(
          bitmap,
          contentDescription = image.fileName,
          contentScale = ContentScale.Fit,
          modifier = Modifier
            .fillMaxSize()
            .pointerInput(image.path) {
              detectTapGestures(onDoubleTap = {
                scale = if (scale > 1f) 1f else DOUBLE_TAP_ZOOM
                offset = Offset.Zero
              })
            }
            .transformable(transform)
            .graphicsLayer(scaleX = scale, scaleY = scale, translationX = offset.x, translationY = offset.y),
        )
        hasFailed -> Text(
          stringResource(R.string.image_unreadable),
          color = Color.White,
          modifier = Modifier.align(Alignment.Center),
        )
        else -> CircularProgressIndicator(Modifier.align(Alignment.Center))
      }
      Row(
        Modifier.fillMaxWidth().background(Color.Black.copy(alpha = 0.5f)).safeDrawingPadding(),
        verticalAlignment = Alignment.CenterVertically,
      ) {
        IconButton(onClick = onDismiss) {
          Icon(Icons.Filled.Close, contentDescription = stringResource(R.string.close), tint = Color.White)
        }
        Text(
          image.fileName,
          color = Color.White,
          maxLines = 1,
          overflow = TextOverflow.Ellipsis,
          style = MaterialTheme.typography.titleSmall,
          modifier = Modifier.weight(1f).padding(horizontal = 8.dp),
        )
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
          TextButton(onClick = { scope.launch { saveToGallery(context, image) } }) {
            Text(stringResource(R.string.image_save), color = Color.White)
          }
        }
        IconButton(onClick = { shareFile(context, File(image.path), image.mediaType) }) {
          Icon(Icons.Filled.Share, contentDescription = stringResource(R.string.share), tint = Color.White)
        }
      }
    }
  }
}

/**
 * Copies the decrypted image into the shared Pictures/TeamCraft folder, the user's explicit choice
 * to take it out of the app. Android 10+ (no storage permission needed there).
 */
@RequiresApi(Build.VERSION_CODES.Q)
private suspend fun saveToGallery(context: Context, image: ViewedImage) {
  val isSaved = withContext(Dispatchers.IO) {
    val resolver = context.contentResolver
    val values = ContentValues().apply {
      put(MediaStore.Images.Media.DISPLAY_NAME, image.fileName)
      put(MediaStore.Images.Media.MIME_TYPE, image.mediaType)
      put(MediaStore.Images.Media.RELATIVE_PATH, "${Environment.DIRECTORY_PICTURES}/$GALLERY_FOLDER")
      put(MediaStore.Images.Media.IS_PENDING, 1)
    }
    val uri = resolver.insert(MediaStore.Images.Media.EXTERNAL_CONTENT_URI, values) ?: return@withContext false
    val isCopied = runCatching {
      resolver.openOutputStream(uri)?.use { output -> File(image.path).inputStream().use { it.copyTo(output) } } != null
    }.getOrDefault(false)
    if (isCopied) {
      resolver.update(uri, ContentValues().apply { put(MediaStore.Images.Media.IS_PENDING, 0) }, null, null)
    } else {
      // A half-written entry would show as a broken picture.
      resolver.delete(uri, null, null)
    }
    isCopied
  }
  Toast.makeText(context, if (isSaved) R.string.image_saved else R.string.image_save_failed, Toast.LENGTH_SHORT).show()
}

/** Hands a decrypted file to another app (send to, save to files) through the FileProvider. */
fun shareFile(context: Context, file: File, mediaType: String) {
  val uri = FileProvider.getUriForFile(context, "${context.packageName}.files", file)
  val intent = Intent(Intent.ACTION_SEND)
    .setType(mediaType)
    .putExtra(Intent.EXTRA_STREAM, uri)
    .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
  runCatching { context.startActivity(Intent.createChooser(intent, file.name)) }
}

/** Decodes with the largest power-of-two sampling that keeps the longer side at `maxSidePx` or more. */
private fun decodeSampled(path: String, maxSidePx: Int): Bitmap? {
  val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
  BitmapFactory.decodeFile(path, bounds)
  if (bounds.outWidth <= 0 || bounds.outHeight <= 0) return null
  var sample = 1
  while (maxOf(bounds.outWidth, bounds.outHeight) / (sample * 2) >= maxSidePx) sample *= 2
  val bitmap = BitmapFactory.decodeFile(path, BitmapFactory.Options().apply { inSampleSize = sample }) ?: return null
  return uprighted(bitmap, path)
}

/** Camera photos are often stored sideways with an EXIF orientation; turn them upright. */
private fun uprighted(bitmap: Bitmap, path: String): Bitmap {
  val orientation = runCatching {
    ExifInterface(path).getAttributeInt(ExifInterface.TAG_ORIENTATION, ExifInterface.ORIENTATION_NORMAL)
  }.getOrDefault(ExifInterface.ORIENTATION_NORMAL)
  val degrees = when (orientation) {
    ExifInterface.ORIENTATION_ROTATE_90 -> 90f
    ExifInterface.ORIENTATION_ROTATE_180 -> 180f
    ExifInterface.ORIENTATION_ROTATE_270 -> 270f
    else -> return bitmap
  }
  val matrix = Matrix().apply { postRotate(degrees) }
  return Bitmap.createBitmap(bitmap, 0, 0, bitmap.width, bitmap.height, matrix, true)
}
