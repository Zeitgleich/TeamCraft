package androidx.media3.decoder.av1

import android.content.Context
import android.os.Handler
import android.os.Looper
import androidx.media3.common.MediaItem
import androidx.media3.common.PlaybackException
import androidx.media3.common.Player
import androidx.media3.common.util.UnstableApi
import androidx.media3.exoplayer.ExoPlayer
import androidx.media3.exoplayer.Renderer
import androidx.media3.exoplayer.RenderersFactory
import androidx.media3.exoplayer.audio.AudioRendererEventListener
import androidx.media3.exoplayer.metadata.MetadataOutput
import androidx.media3.exoplayer.text.TextOutput
import androidx.media3.exoplayer.video.VideoRendererEventListener
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith

/**
 * The app's AV1 decoder (dav1d, decoders/README.md) on a real device: the clips are decoded by it
 * alone, with no platform decoder to fall back on.
 */
@UnstableApi
@RunWith(AndroidJUnit4::class)
class BundledAv1DecoderTest {
  @Before
  fun theLibraryLoads() {
    assertTrue("dav1d JNI library missing", Dav1dLibrary.isAvailable())
  }

  @Test
  fun anEightBitClipPlaysToTheEnd() {
    assertNull(play("asset:///av1-8bit.webm"))
  }

  @Test
  fun aFrameAboveTheSizeLimitIsRefused() {
    val error = play("asset:///av1-too-large.webm")
    assertTrue("expected a decoder error, got $error", error?.errorCode == PlaybackException.ERROR_CODE_DECODING_FAILED)
  }

  /** Plays the clip to its end; the error that stopped it, or null. */
  private fun play(uri: String): PlaybackException? {
    val context = ApplicationProvider.getApplicationContext<Context>()
    val done = CountDownLatch(1)
    var maybeError: PlaybackException? = null
    lateinit var player: ExoPlayer
    Handler(Looper.getMainLooper()).post {
      player = ExoPlayer.Builder(context, Av1Only()).build()
      player.addListener(object : Player.Listener {
        override fun onPlaybackStateChanged(state: Int) {
          if (state == Player.STATE_ENDED) done.countDown()
        }

        override fun onPlayerError(error: PlaybackException) {
          maybeError = error
          done.countDown()
        }
      })
      player.setMediaItem(MediaItem.fromUri(uri))
      player.prepare()
      player.play()
    }
    try {
      assertTrue("playback timed out", done.await(TIMEOUT_S, TimeUnit.SECONDS))
    } finally {
      Handler(Looper.getMainLooper()).post { player.release() }
    }
    return maybeError
  }

  private class Av1Only : RenderersFactory {
    override fun createRenderers(
      eventHandler: Handler,
      videoRendererEventListener: VideoRendererEventListener,
      audioRendererEventListener: AudioRendererEventListener,
      textRendererOutput: TextOutput,
      metadataRendererOutput: MetadataOutput,
    ): Array<Renderer> = arrayOf(Libdav1dVideoRenderer(0, eventHandler, videoRendererEventListener, -1))
  }

  private companion object {
    const val TIMEOUT_S = 30L
  }
}
