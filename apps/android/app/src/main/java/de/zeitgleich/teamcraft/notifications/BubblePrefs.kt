package de.zeitgleich.teamcraft.notifications

import android.app.NotificationManager
import android.content.Context
import android.content.Intent
import android.os.Build
import android.provider.Settings

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

  /**
   * The system lets this app show bubbles: the app's choice in the system settings is "All
   * conversations" or "Selected conversations" (Android 12+), or bubbles are allowed (Android 11).
   * With "Selected", the user picks the conversation with the bubble button of its notification.
   *
   * Channels are not checked: [android.app.NotificationChannel.canBubble] is false for a channel
   * the user never touched, and a direct message may post on the mentions channel.
   */
  fun isAllowedBySystem(): Boolean {
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.R) return false
    val manager = context.getSystemService(NotificationManager::class.java) ?: return false
    return if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
      manager.bubblePreference != NotificationManager.BUBBLE_PREFERENCE_NONE
    } else {
      @Suppress("DEPRECATION") // The only check on Android 11.
      manager.areBubblesAllowed()
    }
  }

  /** The system's bubble settings of this app, where the user turns them on. */
  fun systemSettingsIntent(): Intent = Intent(Settings.ACTION_APP_NOTIFICATION_BUBBLE_SETTINGS)
    .putExtra(Settings.EXTRA_APP_PACKAGE, context.packageName)

  /** A copy, because the set a preference returns must not be changed. */
  private fun enabled(): Set<String> = prefs.getStringSet(KEY_CONVERSATIONS, null)?.toSet().orEmpty()

  companion object {
    private const val FILE = "bubble_prefs"
    private const val KEY_CONVERSATIONS = "conversations"

    /** Conversation bubbles for a messaging app need Android 11. */
    val isSupported: Boolean get() = Build.VERSION.SDK_INT >= Build.VERSION_CODES.R
  }
}
