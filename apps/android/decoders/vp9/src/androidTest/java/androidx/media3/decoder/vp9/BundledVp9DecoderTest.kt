package androidx.media3.decoder.vp9

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
import androidx.media3.exoplayer.video.VideoDecoderGLSurfaceView
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
 * The app's VP9 decoder (libvpx, decoders/README.md) on a real device: the clips are decoded by it
 * alone, with no platform decoder to fall back on. Output goes to an OpenGL view, as in the app's
 * retry path, the only way libvpx shows 10-bit video.
 */
@UnstableApi
@RunWith(AndroidJUnit4::class)
class BundledVp9DecoderTest {
  @Before
  fun theLibraryLoads() {
    assertTrue("libvpx JNI library missing", VpxLibrary.isAvailable())
    assertTrue("libvpx built without 10-bit VP9", VpxLibrary.isHighBitDepthSupported())
  }

  @Test
  fun aProfile0ClipPlaysToTheEnd() {
    assertNull(play("asset:///vp9-profile0.webm"))
  }

  @Test
  fun aTenBitProfile2ClipPlaysToTheEnd() {
    assertNull(play("asset:///vp9-profile2-10bit.webm"))
  }

  @Test
  fun aFrameAboveTheSizeLimitIsRefused() {
    val error = play("asset:///vp9-too-large.webm")
    assertTrue("expected a decoder error, got $error", error?.errorCode == PlaybackException.ERROR_CODE_DECODING_FAILED)
  }

  /** Plays the clip to its end; the error that stopped it, or null. */
  private fun play(uri: String): PlaybackException? {
    val context = ApplicationProvider.getApplicationContext<Context>()
    val done = CountDownLatch(1)
    var maybeError: PlaybackException? = null
    lateinit var player: ExoPlayer
    Handler(Looper.getMainLooper()).post {
      player = ExoPlayer.Builder(context, Vp9Only()).build()
      player.setVideoSurfaceView(VideoDecoderGLSurfaceView(context))
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

  private class Vp9Only : RenderersFactory {
    override fun createRenderers(
      eventHandler: Handler,
      videoRendererEventListener: VideoRendererEventListener,
      audioRendererEventListener: AudioRendererEventListener,
      textRendererOutput: TextOutput,
      metadataRendererOutput: MetadataOutput,
    ): Array<Renderer> = arrayOf(LibvpxVideoRenderer(0, eventHandler, videoRendererEventListener, -1))
  }

  private companion object {
    const val TIMEOUT_S = 30L
  }
}
