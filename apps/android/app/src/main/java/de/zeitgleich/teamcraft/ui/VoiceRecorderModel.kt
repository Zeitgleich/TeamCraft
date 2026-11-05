package de.zeitgleich.teamcraft.ui

import android.content.Context
import android.media.AudioAttributes
import android.media.AudioFocusRequest
import android.media.AudioManager
import android.util.Log
import de.zeitgleich.teamcraft.core.PickedFile
import java.io.File
import java.util.Calendar
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import uniffi.teamcraft_ffi.VoiceException
import uniffi.teamcraft_ffi.VoiceRecording
import uniffi.teamcraft_ffi.VoiceStop
import uniffi.teamcraft_ffi.VoiceTakeInfo
import uniffi.teamcraft_ffi.voiceClearTakes
import uniffi.teamcraft_ffi.voiceFileName

private const val TAG = "VoiceRecorder"

/** How often the level and length are read while the recording shows: ten times a second. */
private const val POLL_MS = 100L

/** Levels kept for the live bars. */
private const val LIVE_BARS = 40

/** The private folder of takes, emptied at every start (a take left by a crash is never sent). */
private const val TAKES_FOLDER = "voice-takes"

/** Which composer a recording belongs to. */
data class VoiceOwner(val conversation: String, val maybeThread: String?)

/** The one recording of the app (#204), as the composer shows it. */
sealed interface VoiceState {
  data object Idle : VoiceState

  /** The microphone is opening. */
  data class Starting(val owner: VoiceOwner, val isLocked: Boolean) : VoiceState

  data class Recording(
    val owner: VoiceOwner,
    /** Hands-free: no finger on the button any more. */
    val isLocked: Boolean,
    val elapsedMs: Long,
    /** The last levels (0..1), oldest first, for the live bars. */
    val levels: List<Float>,
  ) : VoiceState

  /** The file is being finished. */
  data class Finishing(val owner: VoiceOwner) : VoiceState

  /** A take waits for the user to send or discard it; nothing is sent on its own. */
  data class Review(val owner: VoiceOwner, val take: VoiceTakeInfo) : VoiceState

  val maybeOwner: VoiceOwner?
    get() = when (this) {
      Idle -> null
      is Starting -> owner
      is Recording -> owner
      is Finishing -> owner
      is Review -> owner
    }
}

/** Why a recording didn't start; the composer says what to do. */
enum class VoiceRefusal { MICROPHONE, BUSY, FAILED }

/**
 * Records voice messages through the core (`teamcraft-media`: the microphone via AAudio, Opus in
 * Ogg, written to the app's private folder) and keeps the state the composer shows. One recording
 * for the whole app; it belongs to one composer.
 *
 * While recording it holds transient exclusive audio focus: a phone call, another app's recording
 * or the app's own call take the focus and stop the recording, which then waits as a take. So do
 * backgrounding and leaving the conversation (the composer reports those). Nothing is sent until
 * the user says so.
 */
class VoiceRecorderModel(
  private val context: Context,
  private val scope: CoroutineScope,
  isInCall: StateFlow<Boolean>,
  private val stagingDir: suspend () -> File?,
) {
  private val state = MutableStateFlow<VoiceState>(VoiceState.Idle)
  private val refusals = MutableStateFlow<VoiceRefusal?>(null)
  private val lock = Mutex()
  private var maybeRecording: VoiceRecording? = null
  private var maybePoll: Job? = null
  private var maybeFocus: AudioFocusRequest? = null
  private val audio = context.getSystemService(AudioManager::class.java)
  private val takes = File(context.filesDir, TAKES_FOLDER)

  val current: StateFlow<VoiceState> = state

  /** The last refused start, until [refusalShown]. */
  val refusal: StateFlow<VoiceRefusal?> = refusals

  init {
    scope.launch(Dispatchers.IO) {
      takes.mkdirs()
      voiceClearTakes(takes.absolutePath)
    }
    scope.launch {
      isInCall.collect { inCall -> if (inCall) interrupt(VoiceStop.CALL) }
    }
  }

  /** Starts recording for [owner], at most [maxSeconds]; [isLocked] for a tap (hands-free). */
  fun start(owner: VoiceOwner, maxSeconds: Int, isLocked: Boolean, isInCall: Boolean) {
    if (state.value != VoiceState.Idle) return
    if (isInCall) {
      refusals.value = VoiceRefusal.BUSY
      return
    }
    state.value = VoiceState.Starting(owner, isLocked)
    scope.launch {
      lock.withLock {
        if (!requestFocus()) {
          refusals.value = VoiceRefusal.BUSY
          state.value = VoiceState.Idle
          return@withLock
        }
        val path = File(takes, "take-${System.nanoTime()}.ogg").absolutePath
        val result = withContext(Dispatchers.IO) {
          takes.mkdirs()
          runCatching { VoiceRecording.start(path, maxSeconds.toUInt()) }
        }
        result.onSuccess { recording ->
          // Stopped, discarded or interrupted while the microphone opened: nothing was recorded.
          val isStillLocked = whenMicrophoneOpened(state.value)
          if (isStillLocked == null) {
            withContext(Dispatchers.IO) {
              recording.cancel()
              recording.destroy()
            }
            abandonFocus()
            if (state.value is VoiceState.Finishing) state.value = VoiceState.Idle
            return@withLock
          }
          maybeRecording = recording
          state.value = VoiceState.Recording(owner, isStillLocked, 0, emptyList())
          maybePoll = scope.launch { poll(owner) }
        }.onFailure { error ->
          abandonFocus()
          Log.i(TAG, "the voice recording did not start: ${error.javaClass.simpleName}")
          refusals.value = if (error is VoiceException.Microphone) VoiceRefusal.MICROPHONE else VoiceRefusal.FAILED
          state.value = VoiceState.Idle
        }
      }
    }
  }

  /** The finger slid up: the recording goes on hands-free. */
  fun lock() {
    state.value = when (val now = state.value) {
      is VoiceState.Recording -> now.copy(isLocked = true)
      is VoiceState.Starting -> now.copy(isLocked = true)
      else -> now
    }
  }

  /** Stops and keeps the take; [maybeReason] when the user didn't stop it. */
  fun stop(maybeReason: VoiceStop? = null) {
    val owner = when (val now = state.value) {
      is VoiceState.Recording -> now.owner
      is VoiceState.Starting -> now.owner
      else -> return
    }
    state.value = VoiceState.Finishing(owner)
    scope.launch { finish(owner, maybeReason) }
  }

  /** Stops (for [reason]) if recording; a take or nothing stays as it is. */
  fun interrupt(reason: VoiceStop, maybeOwner: VoiceOwner? = null) {
    val now = state.value
    if (now !is VoiceState.Recording && now !is VoiceState.Starting) return
    if (maybeOwner != null && now.maybeOwner != maybeOwner) return
    stop(reason)
  }

  /** Stops and deletes the recording, or the take. */
  fun discard() {
    val before = state.value
    state.value = VoiceState.Idle
    scope.launch {
      lock.withLock {
        maybePoll?.cancel()
        withContext(Dispatchers.IO) {
          maybeRecording?.cancel()
          maybeRecording?.destroy()
          maybeRecording = null
          if (before is VoiceState.Review) File(before.take.path).delete()
        }
        abandonFocus()
      }
    }
  }

  /**
   * The take as a file to send: moved into the core's staging folder, which the core empties once
   * it is uploaded. `null` (and the take stays) when that fails.
   */
  suspend fun takeForSending(): PickedFile? {
    val review = state.value as? VoiceState.Review ?: return null
    val take = review.take
    val maybeFile = withContext(Dispatchers.IO) {
      val directory = stagingDir() ?: return@withContext null
      val now = Calendar.getInstance()
      val name = voiceFileName(
        now.get(Calendar.YEAR),
        (now.get(Calendar.MONTH) + 1).toUInt(),
        now.get(Calendar.DAY_OF_MONTH).toUInt(),
        now.get(Calendar.HOUR_OF_DAY).toUInt(),
        now.get(Calendar.MINUTE).toUInt(),
        now.get(Calendar.SECOND).toUInt(),
      )
      val source = File(take.path)
      val target = File(directory, "${System.nanoTime()}-$name")
      val isMoved = source.renameTo(target) || runCatching {
        source.copyTo(target)
        source.delete()
      }.isSuccess
      if (!isMoved) return@withContext null
      PickedFile(
        fileName = name,
        mediaType = "audio/ogg",
        path = target.absolutePath,
        maybeDurationMs = take.durationMs.toLong(),
        maybeWaveform = take.waveform.map { it.toInt() and 0xff },
      )
    }
    if (maybeFile != null) state.value = VoiceState.Idle
    return maybeFile
  }

  /** The screen's model goes away: a recording is dropped (a take stays until the next start empties the folder). */
  fun close() {
    maybePoll?.cancel()
    maybeRecording?.cancel()
    maybeRecording?.destroy()
    maybeRecording = null
    abandonFocus()
    state.value = VoiceState.Idle
  }

  /** Sign-out or another account: a recording ends without a take, and every take is deleted. */
  suspend fun forgetAll() {
    lock.withLock {
      maybePoll?.cancel()
      state.value = VoiceState.Idle
      withContext(Dispatchers.IO) {
        maybeRecording?.cancel()
        maybeRecording?.destroy()
        maybeRecording = null
        voiceClearTakes(takes.absolutePath)
      }
      abandonFocus()
    }
  }

  fun refusalShown() {
    refusals.value = null
  }

  private suspend fun poll(owner: VoiceOwner) {
    val levels = ArrayDeque<Float>()
    while (scope.isActive) {
      delay(POLL_MS)
      val recording = maybeRecording ?: return
      val now = state.value as? VoiceState.Recording ?: return
      val (level, elapsed, maybeStop) = withContext(Dispatchers.IO) {
        Triple(recording.level(), recording.elapsedMs().toLong(), recording.stoppedByItself())
      }
      levels.addLast(level)
      if (levels.size > LIVE_BARS) levels.removeFirst()
      state.value = now.copy(elapsedMs = elapsed, levels = levels.toList())
      if (maybeStop != null) {
        // The maximum length, or the microphone went: the take waits for the user.
        stop(maybeStop)
        return
      }
    }
  }

  private suspend fun finish(owner: VoiceOwner, maybeReason: VoiceStop?) = lock.withLock {
    maybePoll?.cancel()
    val maybeTake = withContext(Dispatchers.IO) {
      val recording = maybeRecording ?: return@withContext null
      maybeRecording = null
      val result = runCatching { recording.finish(maybeReason) }
      recording.destroy()
      result.onFailure { Log.i(TAG, "the voice recording ended without a take: ${it.javaClass.simpleName}") }
      result.getOrNull()
    }
    abandonFocus()
    // A discard meanwhile wins.
    if (state.value is VoiceState.Finishing) {
      state.value = maybeTake?.let { VoiceState.Review(owner, it) } ?: VoiceState.Idle
    } else {
      maybeTake?.let { withContext(Dispatchers.IO) { File(it.path).delete() } }
    }
  }

  private fun requestFocus(): Boolean {
    val request = AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN_TRANSIENT_EXCLUSIVE)
      .setAudioAttributes(
        AudioAttributes.Builder().setUsage(AudioAttributes.USAGE_MEDIA).setContentType(AudioAttributes.CONTENT_TYPE_SPEECH).build(),
      )
      .setOnAudioFocusChangeListener { change ->
        // A phone call, an alarm or another app took the sound: the take waits.
        if (change < 0) scope.launch { interrupt(VoiceStop.CALL) }
      }
      .build()
    val isGranted = audio.requestAudioFocus(request) == AudioManager.AUDIOFOCUS_REQUEST_GRANTED
    if (isGranted) maybeFocus = request
    return isGranted
  }

  private fun abandonFocus() {
    maybeFocus?.let(audio::abandonAudioFocusRequest)
    maybeFocus = null
  }
}

/**
 * What a recording whose microphone just opened does, by the state meanwhile: still starting, it
 * records (hands-free or held, as the finger left it: the result); anything else (a stop, a
 * discard, an interrupt came first) means `null`, and the recording is dropped.
 */
fun whenMicrophoneOpened(state: VoiceState): Boolean? = (state as? VoiceState.Starting)?.isLocked

/** Whether the app is in a call or one rings: no recording then, and a running one stops. */
fun inCallFlow(call: StateFlow<*>, ring: StateFlow<*>, scope: CoroutineScope): StateFlow<Boolean> {
  val flow = MutableStateFlow(call.value != null || ring.value != null)
  scope.launch { combine(call, ring) { active, ringing -> active != null || ringing != null }.collect { flow.value = it } }
  return flow
}
