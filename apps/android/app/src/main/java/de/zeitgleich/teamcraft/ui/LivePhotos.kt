package de.zeitgleich.teamcraft.ui

import android.net.Uri
import android.util.Log
import androidx.annotation.OptIn
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.media3.common.C
import androidx.media3.common.MediaItem
import androidx.media3.common.PlaybackException
import androidx.media3.common.Player
import androidx.media3.common.util.UnstableApi
import androidx.media3.datasource.FileDataSource
import androidx.media3.exoplayer.ExoPlayer
import androidx.media3.exoplayer.source.ProgressiveMediaSource
import androidx.media3.ui.AspectRatioFrameLayout
import androidx.media3.ui.PlayerView
import de.zeitgleich.teamcraft.R
import java.io.File

private const val TAG = "LivePhotos"

/** The badge's shade over the picture, as the animation badge's. */
private const val BADGE_SCRIM = 0.45f

/**
 * The "Live" badge of a Live Photo or Motion Photo (#2181): says the picture moves, and plays or
 * stops its motion on a tap. Shows a spinner while the clip comes down.
 */
@Composable
fun LiveBadge(isPlaying: Boolean, isComing: Boolean, onClick: () -> Unit, modifier: Modifier = Modifier) {
  val label = stringResource(if (isPlaying) R.string.live_stop else R.string.live_play)
  Row(
    modifier
      .padding(6.dp)
      .heightIn(min = 32.dp)
      .background(Color.Black.copy(alpha = if (isPlaying) BADGE_SCRIM + 0.2f else BADGE_SCRIM), RoundedCornerShape(50))
      .clickable(onClickLabel = label, role = Role.Button, onClick = onClick)
      .semantics { stateDescription = label }
      .padding(start = 6.dp, end = 10.dp, top = 4.dp, bottom = 4.dp),
    verticalAlignment = Alignment.CenterVertically,
    horizontalArrangement = Arrangement.spacedBy(4.dp),
  ) {
    if (isComing) {
      CircularProgressIndicator(Modifier.size(14.dp), color = Color.White, strokeWidth = 2.dp)
    } else {
      Icon(painterResource(R.drawable.ic_live_photo), contentDescription = null, tint = Color.White, modifier = Modifier.size(16.dp))
    }
    Text(stringResource(R.string.live_badge).uppercase(), color = Color.White, style = MaterialTheme.typography.labelSmall)
  }
}

/**
 * The motion of a Live Photo, muted and looping over its picture, from its decrypted file in the
 * app's private downloads. A clip the device can't decode calls [onUnplayable]: the picture stays
 * still, never an error.
 */
@OptIn(UnstableApi::class)
@Composable
fun MotionClip(path: String, onUnplayable: () -> Unit, modifier: Modifier = Modifier, isFilling: Boolean = true) {
  val context = LocalContext.current
  val currentOnUnplayable = rememberUpdatedState(onUnplayable)
  val player = remember(path) {
    ExoPlayer.Builder(context).build().apply {
      volume = 0f
      repeatMode = Player.REPEAT_MODE_ONE
      setMediaSource(
        ProgressiveMediaSource.Factory(FileDataSource.Factory()).createMediaSource(MediaItem.fromUri(Uri.fromFile(File(path)))),
      )
      playWhenReady = true
      prepare()
    }
  }
  DisposableEffect(player) {
    val listener = object : Player.Listener {
      override fun onPlayerError(error: PlaybackException) {
        // The code only: the path names the sender's file.
        Log.i(TAG, "the motion can't be played: ${error.errorCodeName}")
        currentOnUnplayable.value()
      }

      override fun onTracksChanged(tracks: androidx.media3.common.Tracks) {
        if (tracks.containsType(C.TRACK_TYPE_VIDEO) && !tracks.isTypeSupported(C.TRACK_TYPE_VIDEO, true)) currentOnUnplayable.value()
      }
    }
    player.addListener(listener)
    onDispose {
      player.removeListener(listener)
      player.release()
    }
  }
  AndroidView(
    modifier = modifier.fillMaxSize(),
    factory = { viewContext ->
      PlayerView(viewContext).apply {
        useController = false
        resizeMode = if (isFilling) AspectRatioFrameLayout.RESIZE_MODE_ZOOM else AspectRatioFrameLayout.RESIZE_MODE_FIT
        setShutterBackgroundColor(android.graphics.Color.TRANSPARENT)
        this.player = player
      }
    },
  )
}
