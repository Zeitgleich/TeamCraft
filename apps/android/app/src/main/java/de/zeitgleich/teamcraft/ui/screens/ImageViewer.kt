package de.zeitgleich.teamcraft.ui.screens

import android.content.ContentValues
import android.content.Context
import android.content.Intent
import android.os.Build
import android.os.Environment
import android.provider.MediaStore
import android.text.format.DateUtils
import android.text.format.Formatter
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
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.pager.HorizontalPager
import androidx.compose.foundation.pager.rememberPagerState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Share
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.FilterChip
import androidx.compose.material3.FilterChipDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
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
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.style.TextAlign
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
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.ChatActions
import de.zeitgleich.teamcraft.core.ChatSnapshot
import de.zeitgleich.teamcraft.core.FileInfo
import de.zeitgleich.teamcraft.ui.MainViewModel
import de.zeitgleich.teamcraft.ui.MediaPrefs
import de.zeitgleich.teamcraft.ui.OriginalState
import de.zeitgleich.teamcraft.ui.PictureTarget
import de.zeitgleich.teamcraft.ui.PictureViewerState
import de.zeitgleich.teamcraft.ui.Pictures
import de.zeitgleich.teamcraft.ui.THUMBNAIL_DECODE_PX
import de.zeitgleich.teamcraft.ui.rememberThumbnail
import de.zeitgleich.teamcraft.ui.thumbnailKey
import uniffi.teamcraft_ffi.MediaScope

/** The folder under Pictures that saved images go to. */
private const val GALLERY_FOLDER = "TeamCraft"

/** Full images are decoded at this multiple of the screen, so zooming in stays sharp for a while. */
private const val FULL_IMAGE_SCREENS = 2
/** ...but never larger than this on the longer side (GPU texture limits, memory). */
private const val MAX_FULL_IMAGE_PX = 4096

/** Thumbnails smaller than this are blurred while they stand in for the full image. */
private const val SHARP_THUMBNAIL_PX = 480

/** A download's progress is read this often while it runs and the viewer shows it. */
private const val DOWNLOAD_POLL_MS = 1000L

/**
 * The image viewer (#240), full screen over the conversation: the tapped picture at once from its
 * thumbnail, the original swapped in when it is here (fetched by itself or on "Load original",
 * Settings → Chats), with its progress, or "Try again" when it failed (#315). Swipe through the
 * pictures of the message, or of the whole conversation; the ends don't wrap around. Pinch, drag
 * and double tap zoom within the limits every platform shares; a tap shows or hides the bars.
 * Sender and time on top; save, share and forward at the bottom. Only the picture on screen and its
 * neighbours are decoded. System bars are hidden while it is open.
 */
@Composable
fun ImageViewer(viewModel: MainViewModel, snapshot: ChatSnapshot?, target: PictureTarget, onDismiss: () -> Unit) {
  val context = LocalContext.current
  val state = remember(target) { PictureViewerState(target, viewModel.chat) }
  DisposableEffect(state) {
    onDispose {
      state.close()
      Pictures.retainFullImages(emptySet())
    }
  }
  // Saved with the screen: a rotation comes back to the picture and the scope being looked at.
  var resumeKey by rememberSaveable(target.key) { mutableStateOf<String?>(null) }
  var resumeScope by rememberSaveable(target.key) { mutableStateOf<String?>(null) }
  val pager = rememberPagerState { state.items.size }
  val scope = rememberCoroutineScope()
  var isBarShown by remember { mutableStateOf(true) }
  var isZoomed by remember { mutableStateOf(false) }
  var isForwarding by remember { mutableStateOf(false) }
  // A tap on "Load original" or "Try again": the downloads are looked at again.
  var requests by remember { mutableIntStateOf(0) }
  val changes by viewModel.changeCount.collectAsState()
  val prefs = remember { MediaPrefs.prefs(context) }
  val configuration = LocalConfiguration.current
  val density = LocalDensity.current
  val screenPx = with(density) { maxOf(configuration.screenWidthDp, configuration.screenHeightDp).dp.roundToPx() }
  val maxSidePx = (screenPx * FULL_IMAGE_SCREENS).coerceAtMost(MAX_FULL_IMAGE_PX)

  LaunchedEffect(state) {
    // Read before the first page turns the pager (which saves the picture it lands on).
    val maybeResumeKey = resumeKey
    val maybeResumeScope = resumeScope?.let { name -> MediaScope.entries.firstOrNull { it.name == name } }
    state.load()
    maybeResumeKey?.let { key ->
      state.resume(key, maybeResumeScope)
      state.load()
    }
  }
  LaunchedEffect(state, changes) { state.refresh() }
  LaunchedEffect(state.isDownloading) {
    // Downloads report their end in the store, not every chunk: follow them while one runs here.
    while (state.isDownloading) {
      delay(DOWNLOAD_POLL_MS)
      state.refresh()
    }
  }
  LaunchedEffect(state.isGone) { if (state.isGone) onDismiss() }
  LaunchedEffect(state.items) {
    // A new list (the first one, another scope, a page further out): the pager goes to where the
    // core has the user.
    if (state.items.isNotEmpty() && pager.currentPage != state.currentIndex) pager.scrollToPage(state.currentIndex)
  }
  LaunchedEffect(pager, state) {
    snapshotFlow { pager.settledPage }.collect { page ->
      isZoomed = false
      state.show(page)
      state.items.getOrNull(page)?.let { resumeKey = it.key }
      // Not cancelled by the next swipe: a page read halfway would not be asked again.
      scope.launch { state.load() }
    }
  }
  LaunchedEffect(state.currentIndex, state.files, requests) {
    state.nextDownload(prefs.fetchesOriginalNow())?.let { file ->
      viewModel.act(ChatActions.downloadAttachment(file.conversation, file.message, file.fileId))
    }
  }
  LaunchedEffect(state.files, maxSidePx) {
    // Decoded originals of pictures further away than the neighbours are let go.
    val kept = state.files.values.mapNotNull { file -> file.transfer.path?.let { Pictures.fileKey(it, maxSidePx) } }
    Pictures.retainFullImages(kept.toSet())
  }

  Dialog(
    onDismissRequest = onDismiss,
    properties = DialogProperties(usePlatformDefaultWidth = false, decorFitsSystemWindows = false),
  ) {
    ImmersiveDialogWindow()
    Box(Modifier.fillMaxSize().background(Color.Black)) {
      val isAutomatic = remember(state.currentIndex, changes) { prefs.fetchesOriginalNow() }
      if (state.items.isEmpty()) {
        // The opened message is still being read: nothing to page through yet.
        LoadingBadge(null, Modifier.align(Alignment.Center))
      } else {
        HorizontalPager(
          state = pager,
          key = { page -> state.items.getOrNull(page)?.key ?: page },
          // A zoomed picture pans instead of turning the page.
          userScrollEnabled = !isZoomed,
          beyondViewportPageCount = 1,
          modifier = Modifier.fillMaxSize(),
        ) { page ->
          val key = state.items.getOrNull(page)?.key ?: return@HorizontalPager
          val isCurrent = page == pager.currentPage
          PicturePage(
            maybeFile = state.files[key],
            original = state.originalState(key, isAutomatic),
            isCurrent = isCurrent,
            maxSidePx = maxSidePx,
            maxZoom = state.maxZoom,
            doubleTapZoom = state.doubleTapZoom,
            onTap = { isBarShown = !isBarShown },
            onZoomChange = { if (isCurrent) isZoomed = it },
            onFetch = {
              state.request(key)
              requests += 1
            },
          )
        }
      }
      AnimatedVisibility(visible = isBarShown, enter = fadeIn(), exit = fadeOut(), modifier = Modifier.align(Alignment.TopCenter)) {
        TopBar(
          maybeFile = state.currentFile,
          snapshot = snapshot,
          position = state.currentIndex + 1,
          count = state.items.size,
          onDismiss = onDismiss,
        )
      }
      AnimatedVisibility(visible = isBarShown, enter = fadeIn(), exit = fadeOut(), modifier = Modifier.align(Alignment.BottomCenter)) {
        BottomBar(
          maybeFile = state.currentFile,
          maybeScope = state.scope.takeIf { state.canSwitchScope },
          onScope = { chosen ->
            resumeScope = chosen.name
            state.setScope(chosen)
            scope.launch { state.load() }
          },
          onForward = { isForwarding = true },
        )
      }
    }
    val forwarded = state.currentFile
    if (isForwarding && snapshot != null && forwarded != null) {
      ForwardDialog(snapshot, onPick = { to ->
        viewModel.act(ChatActions.forward(forwarded.conversation, forwarded.message, to, ""))
        isForwarding = false
      }, onDismiss = { isForwarding = false })
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

/** Where a page's decoded original stands. */
private sealed interface FullImage {
  data object Waiting : FullImage
  data class Ready(val bitmap: ImageBitmap) : FullImage
  data object Unreadable : FullImage
}

@Composable
private fun PicturePage(
  maybeFile: FileInfo?,
  original: OriginalState,
  isCurrent: Boolean,
  maxSidePx: Int,
  maxZoom: Float,
  doubleTapZoom: Float,
  onTap: () -> Unit,
  onZoomChange: (Boolean) -> Unit,
  onFetch: () -> Unit,
) {
  if (maybeFile == null) {
    // A page further away than the neighbours has no file yet: nothing of it is decoded.
    Box(Modifier.fillMaxSize())
    return
  }
  val file = maybeFile
  val maybePath = (original as? OriginalState.Here)?.path
  val maybeThumbnail by rememberThumbnail(thumbnailKey(file), file.maybeThumbnail?.bytes, THUMBNAIL_DECODE_PX)
  val full by produceState<FullImage>(
    maybePath?.let { path -> Pictures.cached(Pictures.fileKey(path, maxSidePx))?.let { FullImage.Ready(it) } } ?: FullImage.Waiting,
    maybePath,
    maxSidePx,
  ) {
    val path = maybePath ?: return@produceState
    if (value is FullImage.Ready) return@produceState
    value = Pictures.file(path, maxSidePx)?.let { FullImage.Ready(it) } ?: FullImage.Unreadable
  }
  val currentOnTap by rememberUpdatedState(onTap)
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
    zoomTo((previous * zoomChange).coerceIn(1f, maxZoom))
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
          onTap = { currentOnTap() },
          onDoubleTap = { zoomTo(if (scale > 1f) 1f else doubleTapZoom.coerceAtMost(maxZoom)) },
        )
      }
      .transformable(transform, canPan = { scale > 1f }),
    contentAlignment = Alignment.Center,
  ) {
    val decoded = full
    val maybeShown = (decoded as? FullImage.Ready)?.bitmap ?: maybeThumbnail
    maybeShown?.let { bitmap ->
      val isStandIn = decoded !is FullImage.Ready
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
    val overlay = Modifier.align(Alignment.BottomCenter).safeDrawingPadding().padding(bottom = 88.dp)
    when {
      // A format this device can't decode (SVG, AVIF before Android 12, HEIC before 9): another
      // app may open it (issue #337).
      decoded is FullImage.Unreadable -> Column(
        Modifier.align(Alignment.Center).background(Color.Black.copy(alpha = 0.6f), RoundedCornerShape(14.dp)).padding(16.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
      ) {
        Text(stringResource(R.string.image_unreadable), color = Color.White)
        maybePath?.let { path ->
          val context = LocalContext.current
          TextButton(onClick = { openFile(context, File(path), file.mediaType) }) {
            Text(stringResource(R.string.image_open_with), color = Color.White)
          }
        }
      }
      original is OriginalState.Loading -> LoadingBadge(original.maybeProgress, overlay)
      original is OriginalState.Here && decoded is FullImage.Waiting -> LoadingBadge(null, overlay)
      original is OriginalState.OnRequest -> LoadOriginalButton(file, onFetch, overlay)
      original is OriginalState.Failed -> FailedNotice(onFetch, overlay)
      else -> {}
    }
  }
}

/** A small ring over the stand-in while the original comes (the bytes' share once known). */
@Composable
private fun LoadingBadge(maybeProgress: Float?, modifier: Modifier) {
  val description = stringResource(R.string.image_loading)
  val percent = maybeProgress?.let { "${(it * 100).toInt()} %" }
  Box(
    modifier
      .background(Color.Black.copy(alpha = 0.5f), CircleShape)
      .padding(8.dp)
      .semantics {
        contentDescription = description
        percent?.let { stateDescription = it }
      },
  ) {
    if (maybeProgress != null) {
      CircularProgressIndicator(progress = { maybeProgress }, color = Color.White, modifier = Modifier.size(28.dp))
    } else {
      CircularProgressIndicator(color = Color.White, modifier = Modifier.size(28.dp))
    }
  }
}

/** With "never" (or "Wi-Fi only" on mobile data): the original comes on a tap, its size said. */
@Composable
private fun LoadOriginalButton(file: FileInfo, onFetch: () -> Unit, modifier: Modifier) {
  val context = LocalContext.current
  Button(onClick = onFetch, modifier = modifier) {
    Text(stringResource(R.string.image_load_original, Formatter.formatShortFileSize(context, file.sizeBytes)))
  }
}

/** The original didn't come: say so, and offer to try again. */
@Composable
private fun FailedNotice(onRetry: () -> Unit, modifier: Modifier) {
  Column(
    modifier.background(Color.Black.copy(alpha = 0.6f), RoundedCornerShape(14.dp)).padding(horizontal = 16.dp, vertical = 12.dp),
    horizontalAlignment = Alignment.CenterHorizontally,
  ) {
    Text(stringResource(R.string.image_original_failed), color = Color.White, textAlign = TextAlign.Center)
    TextButton(onClick = onRetry) { Text(stringResource(R.string.image_try_again), color = Color.White) }
  }
}

/** Keeps a zoomed picture covering the screen: no dragging its edge past the screen's edge. */
private fun bounded(offset: Offset, scale: Float, size: IntSize): Offset {
  val maxX = size.width * (scale - 1f) / 2f
  val maxY = size.height * (scale - 1f) / 2f
  return Offset(offset.x.coerceIn(-maxX, maxX), offset.y.coerceIn(-maxY, maxY))
}

/** Close, who sent the picture and when, and where it is in the list. */
@Composable
private fun TopBar(maybeFile: FileInfo?, snapshot: ChatSnapshot?, position: Int, count: Int, onDismiss: () -> Unit) {
  val context = LocalContext.current
  Row(
    Modifier.fillMaxWidth().background(Color.Black.copy(alpha = 0.5f)).safeDrawingPadding(),
    verticalAlignment = Alignment.CenterVertically,
  ) {
    IconButton(onClick = onDismiss) {
      Icon(Icons.Filled.Close, contentDescription = stringResource(R.string.close), tint = Color.White)
    }
    Column(Modifier.weight(1f).padding(horizontal = 8.dp).semantics(mergeDescendants = true) {}) {
      val sender = maybeFile?.maybeSender?.let { snapshot?.name(it) }
      Text(
        sender ?: maybeFile?.fileName.orEmpty(),
        color = Color.White,
        maxLines = 1,
        overflow = TextOverflow.Ellipsis,
        style = MaterialTheme.typography.titleSmall,
      )
      maybeFile?.maybeSentAt?.let { sentAt ->
        Text(
          DateUtils.formatDateTime(
            context,
            sentAt,
            DateUtils.FORMAT_SHOW_DATE or DateUtils.FORMAT_SHOW_TIME or DateUtils.FORMAT_ABBREV_MONTH,
          ),
          color = Color.White.copy(alpha = 0.7f),
          style = MaterialTheme.typography.labelSmall,
        )
      }
    }
    if (count > 1) {
      Text(
        stringResource(R.string.image_position, position, count),
        color = Color.White.copy(alpha = 0.7f),
        style = MaterialTheme.typography.labelMedium,
        modifier = Modifier.padding(end = 16.dp),
      )
    }
  }
}

/** The scope switch (an album only), save, share and forward. */
@Composable
private fun BottomBar(
  maybeFile: FileInfo?,
  maybeScope: MediaScope?,
  onScope: (MediaScope) -> Unit,
  onForward: () -> Unit,
) {
  val context = LocalContext.current
  val coroutines = rememberCoroutineScope()
  val maybePath = maybeFile?.transfer?.path?.takeIf { maybeFile.transfer.state == "local" }
  Row(
    Modifier.fillMaxWidth().background(Color.Black.copy(alpha = 0.5f)).safeDrawingPadding().padding(horizontal = 8.dp),
    horizontalArrangement = Arrangement.spacedBy(4.dp),
    verticalAlignment = Alignment.CenterVertically,
  ) {
    maybeScope?.let { scope ->
      ScopeChip(R.string.image_scope_message, scope == MediaScope.ALBUM) { onScope(MediaScope.ALBUM) }
      ScopeChip(R.string.image_scope_conversation, scope == MediaScope.CONVERSATION) { onScope(MediaScope.CONVERSATION) }
    }
    Spacer(Modifier.weight(1f))
    if (maybePath != null && maybeFile != null) {
      if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
        TextButton(onClick = { coroutines.launch { saveToGallery(context, maybePath, maybeFile) } }) {
          Text(stringResource(R.string.image_save), color = Color.White)
        }
      }
      IconButton(onClick = { shareFile(context, File(maybePath), maybeFile.mediaType) }) {
        Icon(Icons.Filled.Share, contentDescription = stringResource(R.string.share), tint = Color.White)
      }
    }
    if (maybeFile != null) {
      IconButton(onClick = onForward) {
        Icon(painterResource(R.drawable.ic_forward), contentDescription = stringResource(R.string.image_forward), tint = Color.White)
      }
    }
  }
}

/** One side of the album/conversation switch. */
@Composable
private fun ScopeChip(label: Int, isSelected: Boolean, onClick: () -> Unit) {
  FilterChip(
    selected = isSelected,
    onClick = onClick,
    label = { Text(stringResource(label)) },
    colors = FilterChipDefaults.filterChipColors(
      labelColor = Color.White,
      selectedLabelColor = Color.Black,
      selectedContainerColor = Color.White,
    ),
  )
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
