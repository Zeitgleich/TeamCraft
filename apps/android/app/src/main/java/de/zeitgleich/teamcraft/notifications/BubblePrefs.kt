package de.zeitgleich.teamcraft.notifications

import android.app.NotificationManager
import android.content.Context
import android.os.Build

/**
 * The conversations the user opted in to show as floating bubbles (issue #591), on this device:
 * bubbles are a feature of the phone's launcher, so the choice does not follow the account. Off by
 * default; a bubble also needs the system to allow them ([isAllowedBySystem]).
 */
class BubblePrefs(private val context: Context) {
  private val prefs = context.getSharedPreferences(FILE, Context.MODE_PRIVATE)

  fun isEnabled(conversationId: String): Boolean = conversationId in enabled()

  fun setEnabled(conversationId: String, isEnabled: Boolean) {
    val updated = if (isEnabled) enabled() + conversationId else enabled() - conversationId
    prefs.edit().putStringSet(KEY_CONVERSATIONS, updated).apply()
  }

  /** The system lets this app show bubbles (Android 11+; the user's switch in the system settings). */
  fun isAllowedBySystem(): Boolean {
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.R) return false
    val manager = context.getSystemService(NotificationManager::class.java) ?: return false
    if (!manager.areBubblesAllowed()) return false
    // The direct messages channel may have been switched off by the user.
    val maybeChannel = manager.getNotificationChannel(NotificationChannels.DIRECT_MESSAGES)
    return maybeChannel?.canBubble() ?: true
  }

  /** A copy, because the set a preference returns must not be changed. */
  private fun enabled(): Set<String> = prefs.getStringSet(KEY_CONVERSATIONS, null)?.toSet().orEmpty()

  companion object {
    private const val FILE = "bubble_prefs"
    private const val KEY_CONVERSATIONS = "conversations"

    /** Conversation bubbles for a messaging app need Android 11. */
    val isSupported: Boolean get() = Build.VERSION.SDK_INT >= Build.VERSION_CODES.R
  }
}
