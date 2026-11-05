package de.zeitgleich.teamcraft.ui.screens

import android.annotation.SuppressLint
import android.net.Uri
import android.text.format.Formatter
import android.util.Log
import android.view.GestureDetector
import android.view.MotionEvent
import androidx.annotation.OptIn
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxScope
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.media3.common.AudioAttributes
import androidx.media3.common.C
import androidx.media3.common.MediaItem
import androidx.media3.common.PlaybackException
import androidx.media3.common.Player
import androidx.media3.common.Tracks
import androidx.media3.common.util.RepeatModeUtil
import androidx.media3.common.util.UnstableApi
import androidx.media3.datasource.FileDataSource
import androidx.media3.exoplayer.DefaultRenderersFactory
import androidx.media3.exoplayer.ExoPlayer
import androidx.media3.exoplayer.source.ProgressiveMediaSource
import androidx.media3.ui.PlayerView
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.ChatActions
import de.zeitgleich.teamcraft.core.FileInfo
import de.zeitgleich.teamcraft.ui.MainViewModel
import de.zeitgleich.teamcraft.ui.PictureTarget
import java.io.File
import kotlinx.coroutines.delay

private const val TAG = "VideoViewer"
/** How far a double tap and the rewind and forward buttons seek. */
private const val SEEK_STEP_MS = 10_000L
private const val CONTROLS_TIMEOUT_MS = 3_000
/** How often a running download is looked at; downloads report their end in the store, not every chunk. */
private const val DOWNLOAD_POLL_MS = 1_000L

/**
 * A video of a message, full screen (#200). The core downloads and decrypts it into the app's
 * private downloads (trimmed at start, wiped with the account), and the player reads that file:
 * only that file, through a file data source, so nothing in a hostile video makes it fetch anything.
 * A format this device can't decode says so and offers another app.
 */
@Composable
fun VideoViewer(viewModel: MainViewModel, target: PictureTarget, onDismiss: () -> Unit) {
  Dialog(
    onDismissRequest = onDismiss,
    properties = DialogProperties(usePlatformDefaultWidth = false, decorFitsSystemWindows = false),
  ) {
    ImmersiveDialogWindow()
    Box(Modifier.fillMaxSize().background(Color.Black)) {
      VideoContent(viewModel, target)
      IconButton(onClick = onDismiss, modifier = Modifier.align(Alignment.TopStart).safeDrawingPadding().padding(4.dp)) {
        Icon(Icons.Filled.Close, contentDescription = stringResource(R.string.close), tint = Color.White)
      }
    }
  }
}

/** The file's state as the core reports it, and what fits it: the player, its download, or why neither. */
@Composable
private fun BoxScope.VideoContent(viewModel: MainViewModel, target: PictureTarget) {
  val changes by viewModel.changeCount.collectAsState()
  var maybeFile by remember(target) { mutableStateOf<FileInfo?>(null) }
  var isRead by remember(target) { mutableStateOf(false) }
  // Saveable: a rotation doesn't ask for the download again, and an unplayable video stays one.
  var hasRequested by rememberSaveable(target.key) { mutableStateOf(false) }
  var isUnplayable by rememberSaveable(target.key) { mutableStateOf(false) }

  suspend fun read() {
    val maybeExtras = viewModel.readChat { extras(target.conversation, listOf(target.message)) } ?: return
    maybeFile = maybeExtras[target.message]?.files?.firstOrNull { it.fileId == target.fileId }
    isRead = true
  }
  LaunchedEffect(target, changes) { read() }
  LaunchedEffect(maybeFile?.transfer?.isMoving) {
    while (maybeFile?.transfer?.isMoving == true) {
      delay(DOWNLOAD_POLL_MS)
      read()
    }
  }
  val download = { viewModel.act(ChatActions.downloadAttachment(target.conversation, target.message, target.fileId)) }
  LaunchedEffect(maybeFile?.transfer?.state) {
    // Opening the video is the request for it: the bubble showed its size and length.
    if (maybeFile?.transfer?.state == "remote" && !hasRequested) {
      hasRequested = true
      download()
    }
  }

  val file = maybeFile
  val maybePath = file?.transfer?.path?.takeIf { file.transfer.state == "local" }
  when {
    file == null -> if (isRead) Notice(stringResource(R.string.file_gone)) else CircularProgressIndicator(Modifier.align(Alignment.Center), color = Color.White)
    isUnplayable -> Notice(stringResource(R.string.video_unplayable)) {
      val context = LocalContext.current
      maybePath?.let { path ->
        TextButton(onClick = { openFile(context, File(path), file.mediaType) }) {
          Text(stringResource(R.string.image_open_with), color = Color.White)
        }
      }
    }
    maybePath != null -> VideoSurface(File(maybePath), file.fileName, onUnplayable = { isUnplayable = true })
    file.transfer.state == "failed" -> Notice(stringResource(R.string.file_failed)) {
      TextButton(onClick = { download() }) { Text(stringResource(R.string.retry), color = Color.White) }
    }
    else -> DownloadProgress(file)
  }
}

/** The poster with the download's progress and the file's size. */
@Composable
private fun BoxScope.DownloadProgress(file: FileInfo) {
  val context = LocalContext.current
  Thumbnail(file, ContentScale.Fit)
  Column(
    Modifier.align(Alignment.Center).background(Color.Black.copy(alpha = 0.6f), RoundedCornerShape(14.dp)).padding(16.dp),
    horizontalAlignment = Alignment.CenterHorizontally,
    verticalArrangement = Arrangement.spacedBy(8.dp),
  ) {
    Text(stringResource(R.string.video_loading, Formatter.formatShortFileSize(context, file.sizeBytes)), color = Color.White)
    file.transfer.maybeProgress?.let { progress ->
      LinearProgressIndicator(progress = { progress }, modifier = Modifier.fillMaxWidth())
    } ?: CircularProgressIndicator(color = Color.White)
  }
}

@Composable
private fun BoxScope.Notice(text: String, action: @Composable () -> Unit = {}) {
  Column(
    Modifier.align(Alignment.Center).padding(24.dp).background(Color.Black.copy(alpha = 0.6f), RoundedCornerShape(14.dp)).padding(16.dp),
    horizontalAlignment = Alignment.CenterHorizontally,
  ) {
    Text(text, color = Color.White, textAlign = TextAlign.Center)
    action()
  }
}

/**
 * Media3's player with its own controls: play and pause, seeking, the scrubber, speed, repeat and
 * captions, translated by the library. The device's decoders do the work, hardware first; a decoder
 * that fails to start falls back to the next one, usually the platform's software decoder.
 */
@OptIn(UnstableApi::class)
@SuppressLint("ClickableViewAccessibility") // The listener only watches for double taps; the view's own click stays.
@Composable
private fun VideoSurface(file: File, label: String, onUnplayable: () -> Unit) {
  val context = LocalContext.current
  val lifecycle = LocalLifecycleOwner.current.lifecycle
  val currentOnUnplayable by rememberUpdatedState(onUnplayable)
  // Saved with the screen: a rotation goes on where it was.
  var positionMs by rememberSaveable(file.path) { mutableLongStateOf(0L) }
  var wasPlaying by rememberSaveable(file.path) { mutableStateOf(true) }
  val player = remember(file) {
    ExoPlayer.Builder(context, DefaultRenderersFactory(context).setEnableDecoderFallback(true))
      .setSeekBackIncrementMs(SEEK_STEP_MS)
      .setSeekForwardIncrementMs(SEEK_STEP_MS)
      .setAudioAttributes(
        AudioAttributes.Builder().setUsage(C.USAGE_MEDIA).setContentType(C.AUDIO_CONTENT_TYPE_MOVIE).build(),
        /* handleAudioFocus = */ true,
      )
      .setHandleAudioBecomingNoisy(true)
      .build()
      .apply {
        val source = ProgressiveMediaSource.Factory(FileDataSource.Factory())
          .createMediaSource(MediaItem.fromUri(Uri.fromFile(file)))
        setMediaSource(source, positionMs)
        playWhenReady = wasPlaying
        prepare()
      }
  }
  DisposableEffect(player, lifecycle) {
    val listener = object : Player.Listener {
      override fun onPlayerError(error: PlaybackException) {
        // The code says enough, and the file's name and content stay out of the logs.
        Log.i(TAG, "the video can't be played: ${error.errorCodeName}")
        currentOnUnplayable()
      }

      override fun onTracksChanged(tracks: Tracks) {
        // Sound alone isn't the video. A size above the decoder's stated limit is still tried.
        if (tracks.containsType(C.TRACK_TYPE_VIDEO) && !tracks.isTypeSupported(C.TRACK_TYPE_VIDEO, true)) currentOnUnplayable()
      }
    }
    val observer = LifecycleEventObserver { _, event ->
      when (event) {
        Lifecycle.Event.ON_PAUSE -> {
          positionMs = player.currentPosition
          wasPlaying = player.playWhenReady
        }
        Lifecycle.Event.ON_STOP -> player.pause()
        else -> {}
      }
    }
    player.addListener(listener)
    lifecycle.addObserver(observer)
    onDispose {
      lifecycle.removeObserver(observer)
      player.removeListener(listener)
      player.release()
    }
  }
  AndroidView(
    modifier = Modifier.fillMaxSize(),
    factory = { viewContext ->
      PlayerView(viewContext).apply {
        contentDescription = label
        this.player = player
        controllerShowTimeoutMs = CONTROLS_TIMEOUT_MS
        setShowBuffering(PlayerView.SHOW_BUFFERING_WHEN_PLAYING)
        setShowNextButton(false)
        setShowPreviousButton(false)
        setShowSubtitleButton(true)
        setRepeatToggleModes(RepeatModeUtil.REPEAT_TOGGLE_MODE_ONE)
        player.addListener(object : Player.Listener {
          override fun onIsPlayingChanged(isPlaying: Boolean) {
            keepScreenOn = isPlaying
          }
        })
        val doubleTaps = GestureDetector(viewContext, object : GestureDetector.SimpleOnGestureListener() {
          override fun onDoubleTap(event: MotionEvent): Boolean {
            player.seekTo((player.currentPosition + doubleTapSeekMs(event.x, width.toFloat())).coerceAtLeast(0L))
            return true
          }
        })
        setOnTouchListener { _, event ->
          doubleTaps.onTouchEvent(event)
          false
        }
      }
    },
  )
}

/** A double tap seeks back on the left half of the video, forward on the right. */
internal fun doubleTapSeekMs(x: Float, width: Float): Long = if (x < width / 2) -SEEK_STEP_MS else SEEK_STEP_MS

/** A video's length as a badge shows it ("0:07", "12:34", "1:02:03"). */
internal fun formatVideoDuration(durationMs: Long): String {
  val total = (durationMs / 1000).coerceAtLeast(0)
  val (hours, minutes, seconds) = Triple(total / 3600, total / 60 % 60, total % 60)
  return if (hours > 0) "%d:%02d:%02d".format(hours, minutes, seconds) else "%d:%02d".format(minutes, seconds)
}

/** The round play mark over a video's poster. */
@Composable
internal fun PlayMark(modifier: Modifier = Modifier) {
  Box(modifier.size(48.dp).background(Color.Black.copy(alpha = 0.55f), CircleShape), contentAlignment = Alignment.Center) {
    Icon(painterResource(R.drawable.ic_play_arrow), contentDescription = null, tint = Color.White, modifier = Modifier.size(28.dp))
  }
}

/** The length and size on a video's poster, known before anything is downloaded. */
@Composable
internal fun VideoBadge(file: FileInfo, modifier: Modifier = Modifier) {
  val size = Formatter.formatShortFileSize(LocalContext.current, file.sizeBytes)
  val text = file.maybeDurationMs?.let { "${formatVideoDuration(it)} · $size" } ?: size
  Text(
    text,
    color = Color.White,
    style = MaterialTheme.typography.labelSmall,
    modifier = modifier.padding(6.dp).background(Color.Black.copy(alpha = 0.55f), RoundedCornerShape(6.dp)).padding(horizontal = 6.dp, vertical = 2.dp),
  )
}
