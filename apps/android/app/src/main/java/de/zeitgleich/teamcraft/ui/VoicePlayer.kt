package de.zeitgleich.teamcraft.ui

import android.content.Context
import android.net.Uri
import android.util.Log
import androidx.annotation.OptIn
import androidx.media3.common.AudioAttributes
import androidx.media3.common.C
import androidx.media3.common.MediaItem
import androidx.media3.common.PlaybackException
import androidx.media3.common.PlaybackParameters
import androidx.media3.common.Player
import androidx.media3.common.util.Log as MediaLog
import androidx.media3.common.util.UnstableApi
import androidx.media3.datasource.FileDataSource
import androidx.media3.exoplayer.ExoPlayer
import androidx.media3.exoplayer.source.ProgressiveMediaSource
import java.io.File
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch

private const val TAG = "VoicePlayer"

/** How often the position is read while playing, for the waveform's progress. */
private const val POSITION_MS = 100L

/** What the voice player holds: which message, where it stands. */
data class VoicePlayback(
  val key: String,
  val isPlaying: Boolean,
  val positionMs: Long,
  val durationMs: Long,
  val hasFailed: Boolean = false,
)

/**
 * The app's one player for voice messages (#204): one plays at a time. ExoPlayer reads the
 * decrypted file in the app's private downloads through a file data source only; it handles audio
 * focus, so a call, a video or another app's sound pauses it, and headphones coming out pause it.
 * The app pauses it when it goes to the background and when its own call starts; playing on the
 * lock screen is left for later.
 */
@OptIn(UnstableApi::class)
class VoicePlayer(private val context: Context, private val scope: CoroutineScope) {
  private val state = MutableStateFlow<VoicePlayback?>(null)
  private var maybePlayer: ExoPlayer? = null
  private var maybeTicker: Job? = null

  val playback: StateFlow<VoicePlayback?> = state

  /** Plays [file] for [key] from [fromFraction] of its length, or pauses and resumes the one held. */
  fun toggle(key: String, file: File, durationHintMs: Long, speed: VoiceSpeed, fromFraction: Float? = null) {
    val now = state.value
    val player = player()
    if (now?.key == key && !now.hasFailed) {
      fromFraction?.let { seekTo(it) }
      if (player.isPlaying && fromFraction == null) player.pause() else player.play()
      return
    }
    // Opening counts as use: the downloads are trimmed least recently used first.
    file.setLastModified(System.currentTimeMillis())
    val source = ProgressiveMediaSource.Factory(FileDataSource.Factory()).createMediaSource(MediaItem.fromUri(Uri.fromFile(file)))
    player.setMediaSource(source, ((fromFraction ?: 0f) * durationHintMs).toLong())
    player.playbackParameters = PlaybackParameters(speed.rate)
    player.prepare()
    player.play()
    state.value = VoicePlayback(key, isPlaying = true, positionMs = 0, durationMs = durationHintMs)
  }

  /** Moves the held message to [fraction] of its length. */
  fun seekTo(fraction: Float) {
    val player = maybePlayer ?: return
    val now = state.value ?: return
    val target = (fraction.coerceIn(0f, 1f) * now.durationMs).toLong()
    player.seekTo(target)
    state.value = now.copy(positionMs = target)
  }

  fun setSpeed(speed: VoiceSpeed) {
    maybePlayer?.playbackParameters = PlaybackParameters(speed.rate)
  }

  fun pause() {
    maybePlayer?.pause()
  }

  /** Lets go of the message: its row left, or the account changes. */
  fun stop() {
    maybePlayer?.stop()
    maybePlayer?.clearMediaItems()
    state.value = null
  }

  fun release() {
    maybeTicker?.cancel()
    maybePlayer?.release()
    maybePlayer = null
    state.value = null
  }

  private fun player(): ExoPlayer = maybePlayer ?: newPlayer()

  private fun newPlayer(): ExoPlayer {
    // Media3's messages can name the file's path; errors are logged by code below.
    MediaLog.setLogLevel(MediaLog.LOG_LEVEL_OFF)
    return ExoPlayer.Builder(context)
      .setAudioAttributes(
        AudioAttributes.Builder().setUsage(C.USAGE_MEDIA).setContentType(C.AUDIO_CONTENT_TYPE_SPEECH).build(),
        // handleAudioFocus
        true,
      )
      .setHandleAudioBecomingNoisy(true)
      .build()
      .also { player ->
        player.addListener(object : Player.Listener {
          override fun onIsPlayingChanged(isPlaying: Boolean) {
            state.value = state.value?.copy(isPlaying = isPlaying, positionMs = player.currentPosition)
            maybeTicker?.cancel()
            if (isPlaying) maybeTicker = scope.launch { follow(player) }
          }

          override fun onPlaybackStateChanged(playbackState: Int) {
            val duration = player.duration
            if (playbackState == Player.STATE_READY && duration != C.TIME_UNSET) {
              state.value = state.value?.copy(durationMs = duration)
            }
            if (playbackState == Player.STATE_ENDED) {
              // Back to the start, ready for the next press.
              player.pause()
              player.seekTo(0)
              state.value = state.value?.copy(isPlaying = false, positionMs = 0)
            }
          }

          override fun onPlayerError(error: PlaybackException) {
            // The code says enough; the file's name and content stay out of the logs.
            Log.i(TAG, "a voice message can't be played: ${error.errorCodeName}")
            state.value = state.value?.copy(isPlaying = false, hasFailed = true)
          }
        })
        maybePlayer = player
      }
  }

  private suspend fun follow(player: ExoPlayer) {
    while (player.isPlaying) {
      state.value = state.value?.copy(positionMs = player.currentPosition)
      delay(POSITION_MS)
    }
  }
}
