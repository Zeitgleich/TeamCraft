package de.zeitgleich.teamcraft.calls

import android.Manifest
import android.annotation.SuppressLint
import android.content.Context
import android.content.pm.PackageManager
import android.graphics.PixelFormat
import android.graphics.Point
import android.hardware.display.DisplayManager
import android.hardware.display.VirtualDisplay
import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioPlaybackCaptureConfiguration
import android.media.AudioRecord
import android.media.ImageReader
import android.media.projection.MediaProjection
import android.os.Build
import android.os.Handler
import android.os.HandlerThread
import android.util.Log
import android.view.Display
import androidx.core.content.ContextCompat
import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import uniffi.teamcraft_ffi.CallSession
import uniffi.teamcraft_ffi.ScreenCaptureSize
import uniffi.teamcraft_ffi.ScreenFrame

private const val TAG = "TeamCraftScreen"
private const val VIRTUAL_DISPLAY_NAME = "TeamCraft screen share"
/** Pictures the reader holds: one being pushed, one arriving. */
private const val READER_IMAGES = 2
private const val NANOS_PER_MICRO = 1_000L
private const val NANOS_PER_SECOND = 1_000_000_000L
/** The share's sound as the engine takes it: 48 kHz mono, 16 bit, 20 ms per chunk. */
private const val AUDIO_RATE_HZ = 48_000
private const val AUDIO_CHUNK_BYTES = AUDIO_RATE_HZ / 50 * 2

/**
 * The phone's screen shared in a call through MediaProjection: a virtual display mirrors the
 * screen into an image reader at the size the engine asks for, and its pictures go to the engine,
 * which converts and encodes them as screen content. Other apps' sound goes along where Android
 * allows capturing it (Android 10+, apps that don't opt out; never the call's own playback).
 *
 * Lives as long as the projection: it ends when the user stops it here or from the system's
 * indicator, when the system revokes the projection, when the call ends, or with the process.
 * The screen turning restarts the share at the new shape, on the same projection.
 */
internal class ScreenCapture(
  private val context: Context,
  private val projection: MediaProjection,
  private val session: CallSession,
  private val scope: CoroutineScope,
  private val onEnded: (ScreenCapture) -> Unit,
) {
  private val thread = HandlerThread("teamcraft-screen").apply { start() }
  private val handler = Handler(thread.looper)
  private val displays = context.getSystemService(DisplayManager::class.java)
  /** Starts, turns and stops one after the other. */
  private val lock = Mutex()
  @Volatile private var isStopped = false
  private var maybeDisplay: VirtualDisplay? = null
  private var maybeReader: ImageReader? = null
  /** Read on the capture thread for every picture, written under [lock]. */
  @Volatile private var maybeSize: ScreenCaptureSize? = null
  /** A turn is being followed; pictures arriving meanwhile don't start another. */
  private val isFollowingTurn = AtomicBoolean(false)
  private var maybeAudio: AudioRecord? = null
  private var maybeAudioThread: Thread? = null
  /** Reused for every picture: the engine copies it before the push returns. */
  private var pixels = ByteArray(0)
  private var lastFrameNanos = Long.MIN_VALUE

  private val projectionCallback = object : MediaProjection.Callback() {
    override fun onStop() {
      // From the system's indicator, the lock screen, or another app's projection.
      Log.i(TAG, "the projection stopped")
      stop()
    }
  }

  private val displayListener = object : DisplayManager.DisplayListener {
    override fun onDisplayAdded(displayId: Int) = Unit

    override fun onDisplayRemoved(displayId: Int) = Unit

    override fun onDisplayChanged(displayId: Int) {
      if (displayId == Display.DEFAULT_DISPLAY) followTurnIfTurned()
    }
  }

  /** Starts sharing; `false` (and everything let go) when the engine refused. */
  suspend fun start(): Boolean = lock.withLock {
    // Android 14+: the callback must be registered before the virtual display is created.
    projection.registerCallback(projectionCallback, handler)
    val screen = screenSize()
    val size = try {
      session.startScreenShare(screen.x.toUInt(), screen.y.toUInt())
    } catch (error: Exception) {
      Log.w(TAG, "the screen share could not be published", error)
      release()
      return false
    }
    // Stopped meanwhile: stop() lets everything go once this returns.
    if (isStopped) return true
    val reader = newReader(size)
    maybeDisplay = try {
      projection.createVirtualDisplay(
        VIRTUAL_DISPLAY_NAME,
        size.width.toInt(),
        size.height.toInt(),
        context.resources.displayMetrics.densityDpi,
        DisplayManager.VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR,
        reader.surface,
        null,
        handler,
      )
    } catch (error: Exception) {
      // A projection the system took back already (Android 14+: one display per consent).
      Log.w(TAG, "no virtual display for the share", error)
      runCatching { session.stopScreenShare() }
      reader.close()
      release()
      return false
    }
    maybeReader = reader
    maybeSize = size
    displays.registerDisplayListener(displayListener, handler)
    startSound()
    return true
  }

  /** Ends the share; safe to call more than once and from any thread. */
  fun stop() {
    if (isStopped) return
    isStopped = true
    // The session may be closed already (the call ended); the engine stopped the share with it.
    runCatching { session.stopScreenShare() }
    scope.launch {
      lock.withLock { release() }
      onEnded(this@ScreenCapture)
    }
  }

  /**
   * Starts [followTurn] when the screen's shape no longer matches the share's. Checked on display
   * changes and on every picture: the display event alone sometimes comes while the display still
   * reports its old size, and a turn back to portrait then went unnoticed (the portrait picture
   * went out pillarboxed in the landscape share until the next turn).
   */
  private fun followTurnIfTurned() {
    val current = maybeSize ?: return
    if (isStopped || !isTurned(current)) return
    if (!isFollowingTurn.compareAndSet(false, true)) return
    scope.launch {
      try {
        followTurn()
      } finally {
        isFollowingTurn.set(false)
      }
    }
  }

  private fun isTurned(current: ScreenCaptureSize): Boolean {
    val screen = screenSize()
    return (screen.x > screen.y) != (current.width > current.height)
  }

  /**
   * The screen turned: the share restarts at the new shape so a landscape picture doesn't go out
   * shrunk into the portrait one. The virtual display only resizes (Android 14+ allows one display
   * per projection).
   */
  private suspend fun followTurn() = lock.withLock {
    val display = maybeDisplay ?: return@withLock
    val current = maybeSize ?: return@withLock
    if (isStopped || !isTurned(current)) return@withLock
    val screen = screenSize()
    val size = try {
      session.startScreenShare(screen.x.toUInt(), screen.y.toUInt())
    } catch (error: Exception) {
      // The engine reports the share as off; the controller stops this capture then.
      Log.w(TAG, "the turned screen could not be shared", error)
      return@withLock
    }
    val reader = newReader(size)
    display.resize(size.width.toInt(), size.height.toInt(), context.resources.displayMetrics.densityDpi)
    display.surface = reader.surface
    maybeReader?.close()
    maybeReader = reader
    maybeSize = size
    Log.i(TAG, "the screen turned; sharing ${size.width}x${size.height}")
  }

  private fun newReader(size: ScreenCaptureSize): ImageReader {
    val reader = ImageReader.newInstance(size.width.toInt(), size.height.toInt(), PixelFormat.RGBA_8888, READER_IMAGES)
    val intervalNanos = NANOS_PER_SECOND / size.maxFramerate.toLong().coerceAtLeast(1)
    reader.setOnImageAvailableListener({ source ->
      // The newest picture only; a still screen delivers none (the engine repeats the last one).
      val image = runCatching { source.acquireLatestImage() }.getOrNull() ?: return@setOnImageAvailableListener
      image.use {
        if (isStopped) return@use
        if (lastFrameNanos != Long.MIN_VALUE && it.timestamp - lastFrameNanos < intervalNanos) return@use
        lastFrameNanos = it.timestamp
        followTurnIfTurned()
        val plane = it.planes[0]
        val buffer = plane.buffer
        buffer.rewind()
        if (pixels.size != buffer.remaining()) pixels = ByteArray(buffer.remaining())
        buffer.get(pixels)
        val frame = ScreenFrame(
          width = it.width.toUInt(),
          height = it.height.toUInt(),
          rgba = pixels,
          rowStride = plane.rowStride.toUInt(),
          timestampUs = (it.timestamp / NANOS_PER_MICRO).toULong(),
        )
        pushOrStop { session.pushScreenFrame(frame) }
      }
    }, handler)
    return reader
  }

  /**
   * The screen's full size as it is turned now, system bars included (what is mirrored). Read from
   * the display, not a window: the share runs without one (the app in the background).
   */
  private fun screenSize(): Point {
    @Suppress("DEPRECATION") // WindowMetrics need a window's context; the display's real size doesn't.
    return Point().also { displays.getDisplay(Display.DEFAULT_DISPLAY).getRealSize(it) }
  }

  /**
   * Other apps' sound, where Android lets it be captured. A share without it still runs: a phone
   * before Android 10, a missing microphone permission, or a capture that fails to start.
   */
  private suspend fun startSound() {
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) return
    val microphone = ContextCompat.checkSelfPermission(context, Manifest.permission.RECORD_AUDIO)
    if (microphone != PackageManager.PERMISSION_GRANTED) return
    val record = playbackCapture() ?: return
    try {
      session.startScreenAudio()
    } catch (error: Exception) {
      Log.w(TAG, "the share's sound could not be sent", error)
      record.release()
      return
    }
    try {
      record.startRecording()
    } catch (error: IllegalStateException) {
      Log.w(TAG, "the share's sound could not be recorded", error)
      runCatching { session.stopScreenAudio() }
      record.release()
      return
    }
    maybeAudio = record
    maybeAudioThread = Thread({ pumpSound(record) }, "teamcraft-screen-sound").apply { start() }
  }

  @SuppressLint("MissingPermission") // Checked by the caller.
  private fun playbackCapture(): AudioRecord? {
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) return null
    val config = AudioPlaybackCaptureConfiguration.Builder(projection)
      .addMatchingUsage(AudioAttributes.USAGE_MEDIA)
      .addMatchingUsage(AudioAttributes.USAGE_GAME)
      .addMatchingUsage(AudioAttributes.USAGE_UNKNOWN)
      .build()
    val format = AudioFormat.Builder()
      .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
      .setSampleRate(AUDIO_RATE_HZ)
      .setChannelMask(AudioFormat.CHANNEL_IN_MONO)
      .build()
    val minBuffer = AudioRecord.getMinBufferSize(AUDIO_RATE_HZ, AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT)
    return try {
      AudioRecord.Builder()
        .setAudioPlaybackCaptureConfig(config)
        .setAudioFormat(format)
        .setBufferSizeInBytes(maxOf(minBuffer, AUDIO_CHUNK_BYTES * 4))
        .build()
        .takeIf { it.state == AudioRecord.STATE_INITIALIZED }
    } catch (error: Exception) {
      Log.w(TAG, "the share's sound can't be captured", error)
      null
    }
  }

  /** Reads the sound in 20 ms chunks until the capture is released. */
  private fun pumpSound(record: AudioRecord) {
    val chunk = ByteArray(AUDIO_CHUNK_BYTES)
    while (!isStopped) {
      val read = record.read(chunk, 0, chunk.size)
      if (read < 0) break
      if (read > 0) pushOrStop { session.pushScreenAudio(if (read == chunk.size) chunk else chunk.copyOf(read)) }
    }
  }

  /**
   * Pushes to the session; a session closed meanwhile (the call ended while a picture was on its
   * way) ends the capture instead of crashing its thread.
   */
  private inline fun pushOrStop(push: () -> Boolean) {
    try {
      push()
    } catch (error: IllegalStateException) {
      Log.i(TAG, "the call ended while sharing", error)
      stop()
    }
  }

  /** Lets everything go; called under the lock. */
  private fun release() {
    isStopped = true
    runCatching { displays.unregisterDisplayListener(displayListener) }
    maybeAudio?.let { record ->
      runCatching { record.stop() }
      maybeAudioThread?.join()
      record.release()
    }
    maybeAudio = null
    maybeAudioThread = null
    maybeDisplay?.release()
    maybeDisplay = null
    maybeReader?.close()
    maybeReader = null
    projection.unregisterCallback(projectionCallback)
    // Ends the system's indicator; stopping twice (the system stopped it) does nothing.
    projection.stop()
    thread.quitSafely()
  }
}
