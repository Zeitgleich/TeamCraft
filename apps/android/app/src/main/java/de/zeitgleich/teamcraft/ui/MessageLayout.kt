package de.zeitgleich.teamcraft.ui

import android.content.Context
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow

/**
 * How a conversation shows its messages: the desktop's message density (#576, #598) on mobile,
 * stored under the same names, a setting of this device. Mobile starts with bubbles.
 */
enum class MessageLayout(val slug: String) {
  /** Chat-app style: own messages on the right in accent bubbles, others on the left. */
  Bubbles("bubbles"),

  /** Every message on the left with its sender's picture, name and time on one line (#1488). */
  Comfortable("comfortable"),

  /** The same list, denser: smaller pictures, less room between senders. */
  Compact("compact"),
  ;

  val isList: Boolean get() = this != Bubbles

  companion object {
    fun fromSlug(slug: String?): MessageLayout = entries.firstOrNull { it.slug == slug } ?: Bubbles
  }
}

/** The stored [MessageLayout]; one instance per process, so an open conversation follows a change at once. */
class MessageLayoutPrefs private constructor(context: Context) {
  private val prefs = context.getSharedPreferences(FILE, Context.MODE_PRIVATE)
  private val state = MutableStateFlow(MessageLayout.fromSlug(prefs.getString(KEY, null)))

  val layout: StateFlow<MessageLayout> = state

  fun setLayout(choice: MessageLayout) {
    state.value = choice
    prefs.edit().putString(KEY, choice.slug).apply()
  }

  companion object {
    private const val FILE = "message_layout"
    private const val KEY = "density"

    @Volatile
    private var shared: MessageLayoutPrefs? = null

    fun of(context: Context): MessageLayoutPrefs =
      shared ?: synchronized(this) { shared ?: MessageLayoutPrefs(context.applicationContext).also { shared = it } }
  }
}
