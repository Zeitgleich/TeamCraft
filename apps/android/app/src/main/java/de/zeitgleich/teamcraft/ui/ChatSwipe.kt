package de.zeitgleich.teamcraft.ui

import android.content.Context
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow

/**
 * What swiping a chat row to the left does (#2007). Swiping right always toggles read; to the left
 * is the favorite star by default, or the notification settings (mute) for people who mute more
 * than they star. A setting of this device.
 */
enum class SwipeEndAction(val slug: String) {
  Favorite("favorite"),
  Mute("mute"),
  ;

  companion object {
    fun fromSlug(slug: String?): SwipeEndAction = entries.firstOrNull { it.slug == slug } ?: Favorite
  }
}

/** The stored [SwipeEndAction]; one instance per process, so the list follows a change at once. */
class ChatSwipePrefs private constructor(context: Context) {
  private val prefs = context.getSharedPreferences(FILE, Context.MODE_PRIVATE)
  private val state = MutableStateFlow(SwipeEndAction.fromSlug(prefs.getString(KEY, null)))

  val endAction: StateFlow<SwipeEndAction> = state

  fun setEndAction(choice: SwipeEndAction) {
    state.value = choice
    prefs.edit().putString(KEY, choice.slug).apply()
  }

  companion object {
    private const val FILE = "chat_swipe"
    private const val KEY = "end_action"

    @Volatile
    private var shared: ChatSwipePrefs? = null

    fun of(context: Context): ChatSwipePrefs =
      shared ?: synchronized(this) { shared ?: ChatSwipePrefs(context.applicationContext).also { shared = it } }
  }
}
