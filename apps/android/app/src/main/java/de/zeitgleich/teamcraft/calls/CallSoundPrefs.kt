package de.zeitgleich.teamcraft.calls

import android.content.Context
import uniffi.teamcraft_ffi.CallStatusSounds

/**
 * Which call status sounds play on this device: a soft repeating tone while the connection is
 * lost, short tones when it is back or the call ended, and a quieter one when someone else drops.
 * All of them by default.
 */
class CallSoundPrefs private constructor(context: Context) {
  private val prefs = context.getSharedPreferences(FILE, Context.MODE_PRIVATE)

  val statusSounds: CallStatusSounds
    get() = when (prefs.getString(KEY_STATUS_SOUNDS, null)) {
      VALUE_OWN_ONLY -> CallStatusSounds.OWN_ONLY
      VALUE_OFF -> CallStatusSounds.OFF
      else -> CallStatusSounds.ALL
    }

  fun setStatusSounds(sounds: CallStatusSounds) {
    val value = when (sounds) {
      CallStatusSounds.ALL -> VALUE_ALL
      CallStatusSounds.OWN_ONLY -> VALUE_OWN_ONLY
      CallStatusSounds.OFF -> VALUE_OFF
    }
    prefs.edit().putString(KEY_STATUS_SOUNDS, value).apply()
  }

  companion object {
    private const val FILE = "call_prefs"
    private const val KEY_STATUS_SOUNDS = "status_sounds"
    private const val VALUE_ALL = "all"
    private const val VALUE_OWN_ONLY = "own_only"
    private const val VALUE_OFF = "off"

    fun prefs(context: Context) = CallSoundPrefs(context.applicationContext)
  }
}
