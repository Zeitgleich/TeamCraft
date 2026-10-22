package de.zeitgleich.teamcraft.security

import android.content.Context

/**
 * "Secure screen": no screenshots, screen recording or app-switcher thumbnails of decrypted chats
 * and calls (`FLAG_SECURE`). A device setting, off by default.
 */
class ScreenPrefs private constructor(context: Context) {
  private val prefs = context.getSharedPreferences(FILE, Context.MODE_PRIVATE)

  val isSecureScreen: Boolean get() = prefs.getBoolean(KEY_SECURE, false)

  fun setSecureScreen(isSecure: Boolean) {
    prefs.edit().putBoolean(KEY_SECURE, isSecure).apply()
  }

  companion object {
    private const val FILE = "screen_prefs"
    private const val KEY_SECURE = "secure_screen"

    fun prefs(context: Context) = ScreenPrefs(context.applicationContext)
  }
}
