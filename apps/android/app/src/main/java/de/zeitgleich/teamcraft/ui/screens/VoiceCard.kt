package de.zeitgleich.teamcraft.ui.screens

import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.detectHorizontalDragGestures
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.CircularProgressIndicator
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
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalLayoutDirection
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.ProgressBarRangeInfo
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.progressBarRangeInfo
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.setProgress
import androidx.compose.ui.unit.LayoutDirection
import androidx.compose.ui.unit.dp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LifecycleEventEffect
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.ChatActions
import de.zeitgleich.teamcraft.core.FileInfo
import de.zeitgleich.teamcraft.ui.MainViewModel
import de.zeitgleich.teamcraft.ui.VoicePrefs
import java.io.File
import uniffi.teamcraft_ffi.voiceWaveformHeights

/** Bars of a voice message's waveform in its card. */
private const val CARD_BARS = 36u

/**
 * A voice message in a bubble (#204): play and pause, the waveform with the played part, tap or
 * drag on it to seek, the time, and the speed (1×, 1.5×, 2×; remembered on the device). The file
 * is downloaded and decrypted on the first press, like any attachment, into the app's private
 * downloads; the app's one voice player plays it, so one message plays at a time.
 */
@Composable
internal fun VoiceCard(viewModel: MainViewModel, file: FileInfo, content: Color, muted: Color, accent: Color) {
  val context = LocalContext.current
  val prefs = remember { VoicePrefs.prefs(context) }
  var speed by remember { mutableStateOf(prefs.speed) }
  val playback by viewModel.voicePlayer.playback.collectAsState()
  val key = "${file.message}/${file.fileId}"
  val current = playback?.takeIf { it.key == key }
  // Pressed while not here yet: plays once the download is done.
  var wantsPlay by remember(key) { mutableStateOf(false) }
  var maybeSeekTo by remember(key) { mutableStateOf<Float?>(null) }
  val heights = remember(file.maybeWaveform) {
    voiceWaveformHeights(file.maybeWaveform.orEmpty().map { it.toByte() }.toByteArray(), CARD_BARS)
  }
  val durationMs = current?.durationMs?.takeIf { it > 0 } ?: file.maybeDurationMs ?: 0L
  val maybePath = file.transfer.path?.takeIf { file.transfer.state == "local" }

  fun play(fromFraction: Float? = null) {
    if (maybePath != null) {
      viewModel.voicePlayer.toggle(key, File(maybePath), durationMs, speed, fromFraction)
    } else {
      wantsPlay = true
      maybeSeekTo = fromFraction
      if (file.transfer.state != "downloading") viewModel.act(ChatActions.downloadAttachment(file.conversation, file.message, file.fileId))
    }
  }
  LaunchedEffect(maybePath, wantsPlay) {
    if (wantsPlay && maybePath != null) {
      wantsPlay = false
      viewModel.voicePlayer.toggle(key, File(maybePath), durationMs, speed, maybeSeekTo)
    }
  }
  LaunchedEffect(file.transfer.state) {
    // A download that failed asks for a new press (the card offers "Try again").
    if (file.transfer.state == "failed") wantsPlay = false
  }
  // The app going to the background pauses it; playing on the lock screen comes later.
  LifecycleEventEffect(Lifecycle.Event.ON_STOP) { if (current != null) viewModel.voicePlayer.pause() }
  DisposableEffect(key) {
    onDispose {
      // No card shows it any more (the row left, the message was deleted): it stops.
      if (viewModel.voicePlayer.playback.value?.key == key) viewModel.voicePlayer.stop()
    }
  }
  val isPlaying = current?.isPlaying == true
  val isLoading = wantsPlay || (file.transfer.state == "downloading" && current == null)
  val played = if (current != null && durationMs > 0) current.positionMs.toFloat() / durationMs else 0f
  val isRtl = LocalLayoutDirection.current == LayoutDirection.Rtl
  fun fractionAt(x: Float, width: Int): Float {
    val fraction = (x / width.coerceAtLeast(1)).coerceIn(0f, 1f)
    return if (isRtl) 1f - fraction else fraction
  }
  val playLabel = stringResource(if (isPlaying) R.string.voice_pause else R.string.voice_play)
  val positionText = stringResource(
    R.string.voice_position,
    formatVideoDuration(current?.positionMs ?: 0),
    formatVideoDuration(durationMs),
  )
  Column(
    Modifier
      .padding(top = 4.dp)
      .widthIn(max = 320.dp)
      .clip(RoundedCornerShape(12.dp))
      .background(content.copy(alpha = 0.08f))
      .padding(horizontal = 4.dp, vertical = 4.dp),
  ) {
    Row(verticalAlignment = Alignment.CenterVertically) {
      IconButton(onClick = { play() }) {
        Box(Modifier.size(40.dp).background(accent, CircleShape), contentAlignment = Alignment.Center) {
          if (isLoading) {
            CircularProgressIndicator(
              progress = { file.transfer.maybeProgress ?: 0f },
              color = MaterialTheme.colorScheme.onPrimary,
              strokeWidth = 2.dp,
              modifier = Modifier.size(24.dp),
            )
          } else {
            Icon(
              painterResource(if (isPlaying) R.drawable.ic_pause else R.drawable.ic_play_arrow),
              contentDescription = playLabel,
              tint = MaterialTheme.colorScheme.onPrimary,
            )
          }
        }
      }
      Column(Modifier.weight(1f).padding(horizontal = 4.dp)) {
        Waveform(
          heights = heights,
          playedFraction = played,
          color = muted,
          playedColor = accent,
          modifier = Modifier
            .height(32.dp)
            .pointerInput(key, maybePath, durationMs) {
              detectTapGestures { offset -> play(fractionAt(offset.x, size.width)) }
            }
            .pointerInput(key, current != null) {
              if (current == null) return@pointerInput
              detectHorizontalDragGestures { change, _ -> viewModel.voicePlayer.seekTo(fractionAt(change.position.x, size.width)) }
            }
            .semantics {
              contentDescription = context.getString(R.string.voice_message)
              progressBarRangeInfo = ProgressBarRangeInfo(played, 0f..1f)
              setProgress(context.getString(R.string.voice_seek)) { target ->
                if (current == null) play(target) else viewModel.voicePlayer.seekTo(target)
                true
              }
            },
        )
        Text(
          if (current != null) "${formatVideoDuration(current.positionMs)} / ${formatVideoDuration(durationMs)}" else formatVideoDuration(durationMs),
          style = MaterialTheme.typography.labelSmall,
          color = muted,
          modifier = Modifier.semantics { contentDescription = positionText },
        )
      }
      TextButton(
        onClick = {
          speed = speed.next()
          prefs.speed = speed
          viewModel.voicePlayer.setSpeed(speed)
        },
        modifier = Modifier.semantics { contentDescription = context.getString(R.string.voice_speed, speed.label) },
      ) {
        Text(speed.label, color = content, style = MaterialTheme.typography.labelLarge)
      }
    }
    if (current?.hasFailed == true) {
      Row(Modifier.padding(horizontal = 8.dp), verticalAlignment = Alignment.CenterVertically) {
        Text(stringResource(R.string.voice_unplayable), color = content, style = MaterialTheme.typography.labelSmall, modifier = Modifier.weight(1f))
        maybePath?.let { path ->
          TextButton(onClick = { openFile(context, File(path), file.mediaType) }) { Text(stringResource(R.string.image_open_with), color = content) }
        }
      }
    } else if (file.transfer.state == "failed" && !wantsPlay) {
      Row(Modifier.padding(horizontal = 8.dp), verticalAlignment = Alignment.CenterVertically) {
        Text(stringResource(R.string.file_failed), color = content, style = MaterialTheme.typography.labelSmall, modifier = Modifier.weight(1f))
        TextButton(onClick = { play() }) { Text(stringResource(R.string.retry), color = content) }
      }
    }
  }
}
