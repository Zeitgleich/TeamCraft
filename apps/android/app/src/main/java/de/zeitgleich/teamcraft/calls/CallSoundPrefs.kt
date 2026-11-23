package de.zeitgleich.teamcraft.calls

import android.content.Context
import de.zeitgleich.teamcraft.settings.LocalSettingChanges
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import uniffi.teamcraft_ffi.CallStatusSounds

/**
 * The call settings of this device. Which call status sounds play: a soft repeating tone while the
 * connection is lost, short tones when it is back or the call ended, and a quieter one when someone
 * else drops; all of them by default. Whether a 1:1 call may connect directly to the other device
 * (#127); on by default. Both follow the account (#945).
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
    LocalSettingChanges.changed()
  }

  /** "Connect directly in calls": the call engine reads it at every join and follows a change. */
  val isDirectAllowed: Boolean
    get() = prefs.getBoolean(KEY_DIRECT_CONNECTIONS, true)

  fun setDirectAllowed(isAllowed: Boolean) {
    prefs.edit().putBoolean(KEY_DIRECT_CONNECTIONS, isAllowed).apply()
    directChanges.value = isAllowed
    LocalSettingChanges.changed()
  }

  companion object {
    private const val FILE = "call_prefs"
    private const val KEY_STATUS_SOUNDS = "status_sounds"
    private const val VALUE_ALL = "all"
    private const val VALUE_OWN_ONLY = "own_only"
    private const val VALUE_OFF = "off"
    private const val KEY_DIRECT_CONNECTIONS = "direct_connections"
    private val directChanges = MutableStateFlow<Boolean?>(null)

    fun prefs(context: Context) = CallSoundPrefs(context.applicationContext)

    /** Each change of [isDirectAllowed] in this process, for a call that runs; `null` before one. */
    val directAllowedChanges: StateFlow<Boolean?> = directChanges
  }
}
