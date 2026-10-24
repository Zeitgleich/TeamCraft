package de.zeitgleich.teamcraft.notifications

import android.content.Context
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import uniffi.teamcraft_ffi.NotificationContent

/**
 * How much message notifications show on this device: the content, the sender only, or nothing
 * but "New message" (issue #314). The core applies it (with the workspace policy as a limit) when it
 * opens a wake-up; [MessageNotifier] applies it again when it posts, so a change also hides what is
 * already in the shade. The app lock caps it at the sender ([MessagePrivacy]).
 */
class NotificationContentPrefs(context: Context) {
  private val prefs = context.getSharedPreferences(FILE, Context.MODE_PRIVATE)
  private val state = MutableStateFlow(read())

  val content: StateFlow<NotificationContent> = state

  fun update(content: NotificationContent) {
    prefs.edit().putString(KEY_CONTENT, content.name).apply()
    state.value = content
  }

  private fun read(): NotificationContent =
    NotificationContent.entries.firstOrNull { it.name == prefs.getString(KEY_CONTENT, null) } ?: NotificationContent.CONTENT

  private companion object {
    const val FILE = "notification_content"
    const val KEY_CONTENT = "content"
  }
}
