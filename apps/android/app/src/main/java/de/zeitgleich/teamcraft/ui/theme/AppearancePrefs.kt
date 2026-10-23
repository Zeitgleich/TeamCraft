package de.zeitgleich.teamcraft.ui.theme

import android.content.Context
import android.os.Build
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow

/**
 * "Use the colors of the wallpaper" (Material You, Android 12+) instead of the TeamCraft colors.
 * A device setting, off by default so the app looks like desktop and web.
 */
class AppearancePrefs private constructor(context: Context) {
  private val prefs = context.getSharedPreferences(FILE, Context.MODE_PRIVATE)
  private val dynamic = shared ?: MutableStateFlow(prefs.getBoolean(KEY_DYNAMIC, false)).also { shared = it }

  /** Live, so the theme switches as soon as the setting changes. */
  val isDynamicColor: StateFlow<Boolean> = dynamic

  fun setDynamicColor(isOn: Boolean) {
    prefs.edit().putBoolean(KEY_DYNAMIC, isOn).apply()
    dynamic.value = isOn
  }

  companion object {
    private const val FILE = "appearance_prefs"
    private const val KEY_DYNAMIC = "dynamic_color"

    /** One flow per process: every screen and the theme see the same value. */
    private var shared: MutableStateFlow<Boolean>? = null

    val isSupported: Boolean get() = Build.VERSION.SDK_INT >= Build.VERSION_CODES.S

    fun prefs(context: Context) = AppearancePrefs(context.applicationContext)
  }
}
