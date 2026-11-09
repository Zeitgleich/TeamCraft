package de.zeitgleich.teamcraft.ui

import android.content.Context
import de.zeitgleich.teamcraft.core.SendAttempt
import de.zeitgleich.teamcraft.core.SendAttemptStore

/**
 * The composers' unconfirmed sends ([de.zeitgleich.teamcraft.core.ComposerSend]) in app-private
 * preferences: an ID and a digest each, never the text. Wiped with the account's data at sign-out.
 */
class SendAttemptPrefs(context: Context) : SendAttemptStore {
  private val prefs = context.applicationContext.getSharedPreferences(FILE, Context.MODE_PRIVATE)

  override fun load(key: String): SendAttempt? {
    val stored = prefs.getString(KEY_PREFIX + key, null) ?: return null
    val id = stored.substringBefore(' ')
    val fingerprint = stored.substringAfter(' ', "")
    return SendAttempt(id, fingerprint).takeIf { id.isNotEmpty() && fingerprint.isNotEmpty() }
  }

  override fun save(key: String, attempt: SendAttempt) {
    prefs.edit().putString(KEY_PREFIX + key, "${attempt.clientMessageId} ${attempt.fingerprint}").apply()
  }

  override fun clear(key: String) {
    prefs.edit().remove(KEY_PREFIX + key).apply()
  }

  /** Sign-out: no attempt outlives the session on this device. */
  fun wipe() {
    prefs.edit().clear().apply()
  }

  private companion object {
    const val FILE = "composer_send_attempts"
    const val KEY_PREFIX = "attempt:"
  }
}
