package de.zeitgleich.teamcraft.ui

import android.content.Context

/**
 * Composer preferences of this device. "Send with Enter" suits hardware keyboards and tablets;
 * off by default, so Enter makes a new line as in most messengers on phones.
 */
class ComposerPrefs private constructor(context: Context) {
  private val prefs = context.getSharedPreferences(FILE, Context.MODE_PRIVATE)

  val isSendWithEnter: Boolean get() = prefs.getBoolean(KEY_SEND_WITH_ENTER, false)

  fun setSendWithEnter(isOn: Boolean) {
    prefs.edit().putBoolean(KEY_SEND_WITH_ENTER, isOn).apply()
  }

  companion object {
    private const val FILE = "composer_prefs"
    private const val KEY_SEND_WITH_ENTER = "send_with_enter"

    fun prefs(context: Context) = ComposerPrefs(context.applicationContext)
  }
}
