package de.zeitgleich.teamcraft.ui

import android.content.Context
import de.zeitgleich.teamcraft.settings.LocalSettingChanges
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow

/**
 * The order the user put their favorites in (#2057), the same list desktop and web keep: it follows
 * the account (`favorite_order`). Favorites not in it come after those that are, in the section's sort.
 */
class FavoriteOrder private constructor(context: Context) {
  private val prefs = context.getSharedPreferences(FILE, Context.MODE_PRIVATE)
  private val state = MutableStateFlow(read())

  val order: StateFlow<List<String>> = state

  /** Stores [ids] (conversation IDs, first on top); a change of the user reaches the account. */
  fun set(ids: List<String>) {
    if (ids == state.value) return
    prefs.edit().putString(KEY, ids.joinToString("\n")).apply()
    state.value = ids
    LocalSettingChanges.changed()
  }

  private fun read(): List<String> = prefs.getString(KEY, "").orEmpty().lines().filter { it.isNotBlank() }

  companion object {
    private const val FILE = "favorite_order"
    private const val KEY = "ids"

    @Volatile private var maybeInstance: FavoriteOrder? = null

    fun of(context: Context): FavoriteOrder =
      maybeInstance ?: synchronized(this) { maybeInstance ?: FavoriteOrder(context.applicationContext).also { maybeInstance = it } }

    /**
     * The order after [id] moved one place [up] or down among [displayed] (the favorites as shown),
     * or null when it can't (not shown, or already at that end).
     */
    fun moved(displayed: List<String>, id: String, up: Boolean): List<String>? {
      val at = displayed.indexOf(id)
      val to = if (up) at - 1 else at + 1
      if (at < 0 || to !in displayed.indices) return null
      return displayed.toMutableList().also { it.add(to, it.removeAt(at)) }
    }
  }
}
