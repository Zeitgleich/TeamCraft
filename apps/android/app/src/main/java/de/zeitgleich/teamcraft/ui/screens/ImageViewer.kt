package de.zeitgleich.teamcraft.ui.screens

import android.content.ContentValues
import android.content.Context
import android.content.Intent
import android.os.Build
import android.os.Environment
import android.provider.MediaStore
import android.view.WindowManager
import android.widget.Toast
import androidx.annotation.RequiresApi
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.gestures.rememberTransformableState
import androidx.compose.foundation.gestures.transformable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.pager.HorizontalPager
import androidx.compose.foundation.pager.rememberPagerState
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Share
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.blur
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.compose.ui.window.DialogWindowProvider
import androidx.core.content.FileProvider
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import java.io.File
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.FileInfo
import de.zeitgleich.teamcraft.ui.Pictures
import de.zeitgleich.teamcraft.ui.THUMBNAIL_DECODE_PX
import de.zeitgleich.teamcraft.ui.rememberThumbnail
import de.zeitgleich.teamcraft.ui.thumbnailKey

/** The folder under Pictures that saved images go to. */
private const val GALLERY_FOLDER = "TeamCraft"

/** Zoom limits of the viewer; double tap toggles between 1× and this. */
private const val MAX_ZOOM = 5f
private const val DOUBLE_TAP_ZOOM = 2.5f

/** Full images are decoded at this multiple of the screen, so zooming in stays sharp for a while. */
private const val FULL_IMAGE_SCREENS = 2
/** ...but never larger than this on the longer side (GPU texture limits, memory). */
private const val MAX_FULL_IMAGE_PX = 4096

/** Thumbnails smaller than this are blurred while they stand in for the full image. */
private const val SHARP_THUMBNAIL_PX = 480

/**
 * Full-screen images, one page each: swipe between the pictures of the conversation, pinch to zoom,
 * drag to pan, double tap to zoom in and out, tap to show or hide the bar. A page shows the sender's
 * thumbnail at once and swaps in the full image when it is downloaded and decrypted (started here),
 * so opening never waits on a black screen (#315). System bars are hidden while the viewer is open.
 */
@Composable
fun ImageViewer(images: List<FileInfo>, startFileId: String, onDownload: (FileInfo) -> Unit, onDismiss: () -> Unit) {
  val startPage = remember { images.indexOfFirst { it.fileId == startFileId }.coerceAtLeast(0) }
  val pager = rememberPagerState(initialPage = startPage) { images.size }
  var isBarShown by remember { mutableStateOf(true) }
  var isZoomed by remember { mutableStateOf(false) }
  val configuration = LocalConfiguration.current
  val density = LocalDensity.current
  val screenPx = with(density) { maxOf(configuration.screenWidthDp, configuration.screenHeightDp).dp.roundToPx() }
  val maxSidePx = (screenPx * FULL_IMAGE_SCREENS).coerceAtMost(MAX_FULL_IMAGE_PX)
  Dialog(
    onDismissRequest = onDismiss,
    properties = DialogProperties(usePlatformDefaultWidth = false, decorFitsSystemWindows = false),
  ) {
    ImmersiveDialogWindow()
    Box(Modifier.fillMaxSize().background(Color.Black)) {
      HorizontalPager(
        state = pager,
        key = { images[it].fileId },
        // A zoomed picture pans instead of turning the page.
        userScrollEnabled = !isZoomed,
        beyondViewportPageCount = 1,
        modifier = Modifier.fillMaxSize(),
      ) { page ->
        val isCurrent = page == pager.currentPage
        ImagePage(
          file = images[page],
          isCurrent = isCurrent,
          maxSidePx = maxSidePx,
          onDownload = onDownload,
          onTap = { isBarShown = !isBarShown },
          onZoomChange = { if (isCurrent) isZoomed = it },
        )
      }
      images.getOrNull(pager.currentPage)?.let { file ->
        AnimatedVisibility(visible = isBarShown, enter = fadeIn(), exit = fadeOut()) {
          ViewerBar(file, position = pager.currentPage + 1, count = images.size, onDismiss = onDismiss)
        }
      }
    }
  }
}

/**
 * Hides the status and navigation bars while the viewer shows (a swipe from the edge brings them
 * back for a moment) and lets the picture use the cutout area. The bars return when it closes.
 */
@Composable
private fun ImmersiveDialogWindow() {
  val view = LocalView.current
  DisposableEffect(view) {
    val window = (view.parent as? DialogWindowProvider)?.window ?: return@DisposableEffect onDispose {}
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
      window.attributes = window.attributes.apply {
        layoutInDisplayCutoutMode = WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_SHORT_EDGES
      }
    }
    val controller = WindowCompat.getInsetsController(window, view)
    controller.systemBarsBehavior = WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
    controller.hide(WindowInsetsCompat.Type.systemBars())
    onDispose { controller.show(WindowInsetsCompat.Type.systemBars()) }
  }
}

/** Where a page's full image stands. */
private sealed interface FullImage {
  data object Waiting : FullImage
  data class Ready(val bitmap: ImageBitmap) : FullImage
  data object Unreadable : FullImage
}

@Composable
private fun ImagePage(
  file: FileInfo,
  isCurrent: Boolean,
  maxSidePx: Int,
  onDownload: (FileInfo) -> Unit,
  onTap: () -> Unit,
  onZoomChange: (Boolean) -> Unit,
) {
  val maybePath = file.transfer.path?.takeIf { file.transfer.state == "local" }
  val maybeThumbnail by rememberThumbnail(thumbnailKey(file), file.maybeThumbnail?.bytes, THUMBNAIL_DECODE_PX)
  val full by produceState<FullImage>(
    maybePath?.let { path -> Pictures.cached(Pictures.fileKey(path, maxSidePx))?.let { FullImage.Ready(it) } } ?: FullImage.Waiting,
    maybePath,
  ) {
    val path = maybePath ?: return@produceState
    if (value is FullImage.Ready) return@produceState
    value = Pictures.file(path, maxSidePx)?.let { FullImage.Ready(it) } ?: FullImage.Unreadable
  }
  LaunchedEffect(isCurrent, file.transfer.state) {
    // Only the picture being looked at is fetched; a failed one waits for a tap.
    if (isCurrent && file.transfer.state == "remote") onDownload(file)
  }
  var scale by remember { mutableFloatStateOf(1f) }
  var offset by remember { mutableStateOf(Offset.Zero) }
  var size by remember { mutableStateOf(IntSize.Zero) }
  fun zoomTo(next: Float) {
    scale = next
    if (next == 1f) offset = Offset.Zero
    onZoomChange(next > 1f)
  }
  LaunchedEffect(isCurrent) {
    // A page swiped away comes back unzoomed.
    if (!isCurrent && scale != 1f) zoomTo(1f)
  }
  val transform = rememberTransformableState { centroid: Offset, zoomChange: Float, pan: Offset, _: Float ->
    val previous = scale
    zoomTo((previous * zoomChange).coerceIn(1f, MAX_ZOOM))
    // Zoom around the fingers: the point under them stays under them.
    val zoom = scale / previous
    val fromCenter = centroid - Offset(size.width / 2f, size.height / 2f)
    if (scale > 1f) offset = bounded(offset * zoom + fromCenter * (1f - zoom) + pan, scale, size)
  }
  Box(
    Modifier
      .fillMaxSize()
      .onSizeChanged { size = it }
      .pointerInput(file.fileId) {
        detectTapGestures(
          onTap = {
            if (file.transfer.state == "failed") onDownload(file) else onTap()
          },
          onDoubleTap = { zoomTo(if (scale > 1f) 1f else DOUBLE_TAP_ZOOM) },
        )
      }
      .transformable(transform, canPan = { scale > 1f }),
    contentAlignment = Alignment.Center,
  ) {
    val state = full
    val maybeShown = (state as? FullImage.Ready)?.bitmap ?: maybeThumbnail
    maybeShown?.let { bitmap ->
      val isStandIn = state !is FullImage.Ready
      val isBlurry = isStandIn && maxOf(bitmap.width, bitmap.height) < SHARP_THUMBNAIL_PX
      Image(
        bitmap,
        contentDescription = file.fileName,
        contentScale = ContentScale.Fit,
        modifier = Modifier
          .fillMaxSize()
          .then(if (isBlurry) Modifier.blur(12.dp) else Modifier)
          .graphicsLayer(scaleX = scale, scaleY = scale, translationX = offset.x, translationY = offset.y),
      )
    }
    when {
      state is FullImage.Unreadable -> Text(stringResource(R.string.image_unreadable), color = Color.White)
      file.transfer.state == "failed" -> Text(stringResource(R.string.image_load_failed), color = Color.White)
      state is FullImage.Waiting -> LoadingBadge(file, Modifier.align(Alignment.BottomCenter))
      else -> {}
    }
  }
}

/** A small spinner over the stand-in while the full image comes (the bytes' share once known). */
@Composable
private fun LoadingBadge(file: FileInfo, modifier: Modifier) {
  val description = stringResource(R.string.image_loading)
  Box(modifier.safeDrawingPadding().padding(24.dp).semantics { contentDescription = description }) {
    val maybeProgress = file.transfer.maybeProgress
    if (maybeProgress != null) {
      CircularProgressIndicator(progress = { maybeProgress }, color = Color.White, modifier = Modifier.size(32.dp))
    } else {
      CircularProgressIndicator(color = Color.White, modifier = Modifier.size(32.dp))
    }
  }
}

/** Keeps a zoomed picture covering the screen: no dragging its edge past the screen's edge. */
private fun bounded(offset: Offset, scale: Float, size: IntSize): Offset {
  val maxX = size.width * (scale - 1f) / 2f
  val maxY = size.height * (scale - 1f) / 2f
  return Offset(offset.x.coerceIn(-maxX, maxX), offset.y.coerceIn(-maxY, maxY))
}

@Composable
private fun ViewerBar(file: FileInfo, position: Int, count: Int, onDismiss: () -> Unit) {
  val context = LocalContext.current
  val scope = rememberCoroutineScope()
  val maybePath = file.transfer.path?.takeIf { file.transfer.state == "local" }
  Row(
    Modifier.fillMaxWidth().background(Color.Black.copy(alpha = 0.5f)).safeDrawingPadding(),
    verticalAlignment = Alignment.CenterVertically,
  ) {
    IconButton(onClick = onDismiss) {
      Icon(Icons.Filled.Close, contentDescription = stringResource(R.string.close), tint = Color.White)
    }
    Column(Modifier.weight(1f).padding(horizontal = 8.dp)) {
      Text(
        file.fileName,
        color = Color.White,
        maxLines = 1,
        overflow = TextOverflow.Ellipsis,
        style = MaterialTheme.typography.titleSmall,
      )
      if (count > 1) {
        Text(
          stringResource(R.string.image_position, position, count),
          color = Color.White.copy(alpha = 0.7f),
          style = MaterialTheme.typography.labelSmall,
        )
      }
    }
    if (maybePath != null) {
      if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
        TextButton(onClick = { scope.launch { saveToGallery(context, maybePath, file) } }) {
          Text(stringResource(R.string.image_save), color = Color.White)
        }
      }
      IconButton(onClick = { shareFile(context, File(maybePath), file.mediaType) }) {
        Icon(Icons.Filled.Share, contentDescription = stringResource(R.string.share), tint = Color.White)
      }
    }
  }
}

/**
 * Copies the decrypted image into the shared Pictures/TeamCraft folder, the user's explicit choice
 * to take it out of the app. Android 10+ (no storage permission needed there).
 */
@RequiresApi(Build.VERSION_CODES.Q)
private suspend fun saveToGallery(context: Context, path: String, file: FileInfo) {
  val isSaved = withContext(Dispatchers.IO) {
    val resolver = context.contentResolver
    val values = ContentValues().apply {
      put(MediaStore.Images.Media.DISPLAY_NAME, file.fileName)
      put(MediaStore.Images.Media.MIME_TYPE, file.mediaType)
      put(MediaStore.Images.Media.RELATIVE_PATH, "${Environment.DIRECTORY_PICTURES}/$GALLERY_FOLDER")
      put(MediaStore.Images.Media.IS_PENDING, 1)
    }
    val uri = resolver.insert(MediaStore.Images.Media.EXTERNAL_CONTENT_URI, values) ?: return@withContext false
    val isCopied = runCatching {
      resolver.openOutputStream(uri)?.use { output -> File(path).inputStream().use { it.copyTo(output) } } != null
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
