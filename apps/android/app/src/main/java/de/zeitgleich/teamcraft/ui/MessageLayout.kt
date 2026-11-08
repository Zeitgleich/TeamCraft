package de.zeitgleich.teamcraft.ui

import android.content.Context
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow

/**
 * How a conversation shows its messages (#1488), the desktop's choice between bubbles and the
 * list: a setting of this device, as on desktop and web.
 */
enum class MessageLayout {
  /** Chat-app style: own messages on the right in accent bubbles, others on the left. */
  Bubbles,

  /** Every message on the left with its sender's picture, name and time on one line. */
  List,
}

/** The stored [MessageLayout]; one instance per process, so an open conversation follows a change at once. */
class MessageLayoutPrefs private constructor(context: Context) {
  private val prefs = context.getSharedPreferences(FILE, Context.MODE_PRIVATE)
  private val state = MutableStateFlow(
    MessageLayout.entries.firstOrNull { it.name == prefs.getString(KEY, null) } ?: MessageLayout.Bubbles,
  )

  val layout: StateFlow<MessageLayout> = state

  fun setLayout(choice: MessageLayout) {
    state.value = choice
    prefs.edit().putString(KEY, choice.name).apply()
  }

  companion object {
    private const val FILE = "message_layout"
    private const val KEY = "layout"

    @Volatile
    private var shared: MessageLayoutPrefs? = null

    fun of(context: Context): MessageLayoutPrefs =
      shared ?: synchronized(this) { shared ?: MessageLayoutPrefs(context.applicationContext).also { shared = it } }
  }
}
