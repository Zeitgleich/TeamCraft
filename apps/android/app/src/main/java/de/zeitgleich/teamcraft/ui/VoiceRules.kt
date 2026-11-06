package de.zeitgleich.teamcraft.ui

import android.content.Context
import de.zeitgleich.teamcraft.R
import uniffi.teamcraft_ffi.VoiceStop

/**
 * The plain rules of voice messages (#204) on Android, apart from the UI so they can be tested:
 * the playback speeds, the hold-to-record gesture and the words for a stopped recording.
 */
enum class VoiceSpeed(val rate: Float, val label: String) {
  NORMAL(1f, "1×"),
  ONE_AND_HALF(1.5f, "1.5×"),
  DOUBLE(2f, "2×"),
  ;

  /** The speed a tap on the speed button goes to. */
  fun next(): VoiceSpeed = entries[(ordinal + 1) % entries.size]

  companion object {
    fun of(name: String?): VoiceSpeed = entries.firstOrNull { it.name == name } ?: NORMAL
  }
}

/** Where a finger held on the record button has gone. */
enum class HoldGesture { RECORDING, CANCEL, LOCK }

object VoiceGestures {
  /** A press shorter than this is a tap: hands-free recording. Longer, it records while held. */
  const val HOLD_DELAY_MS = 250L

  /** Slid this far towards the start (dp) and let go, the recording is discarded. */
  const val CANCEL_DP = 96f

  /** Slid this far up (dp), the recording goes on hands-free. */
  const val LOCK_DP = 72f

  /** Where a hold stands after the finger moved by [dx] and [dy] dp (up is negative); in RTL layouts [dx] is mirrored by the caller. */
  fun of(dx: Float, dy: Float): HoldGesture = when {
    -dy >= LOCK_DP -> HoldGesture.LOCK
    -dx >= CANCEL_DP -> HoldGesture.CANCEL
    else -> HoldGesture.RECORDING
  }
}

/** Device preferences of voice messages: the playback speed, remembered across messages. */
class VoicePrefs private constructor(context: Context) {
  private val prefs = context.getSharedPreferences(FILE, Context.MODE_PRIVATE)

  var speed: VoiceSpeed
    get() = VoiceSpeed.of(prefs.getString(KEY_SPEED, null))
    set(value) = prefs.edit().putString(KEY_SPEED, value.name).apply()

  /** The microphone permission was asked once: a refusal without a rationale since is for good. */
  var hasAskedMicrophone: Boolean
    get() = prefs.getBoolean(KEY_ASKED_MICROPHONE, false)
    set(value) = prefs.edit().putBoolean(KEY_ASKED_MICROPHONE, value).apply()

  companion object {
    private const val FILE = "voice_prefs"
    private const val KEY_SPEED = "speed"
    private const val KEY_ASKED_MICROPHONE = "asked_microphone"

    fun prefs(context: Context) = VoicePrefs(context.applicationContext)
  }
}

/** Why a recording stopped before the user sent it, in words; `null` when the user stopped it. */
fun voiceStopText(context: Context, maybeStop: VoiceStop?): String? = when (maybeStop) {
  null -> null
  VoiceStop.LIMIT_REACHED -> context.getString(R.string.voice_stop_limit)
  VoiceStop.CALL -> context.getString(R.string.voice_stop_call)
  VoiceStop.BACKGROUND -> context.getString(R.string.voice_stop_background)
  VoiceStop.MICROPHONE_LOST -> context.getString(R.string.voice_stop_microphone)
  VoiceStop.LEFT -> context.getString(R.string.voice_stop_left)
}
