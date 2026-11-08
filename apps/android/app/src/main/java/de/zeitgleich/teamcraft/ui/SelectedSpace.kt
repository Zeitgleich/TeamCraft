package de.zeitgleich.teamcraft.ui

import android.content.Context
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow

/**
 * The space Home of the spaces layout shows (#1526), remembered on this device as the desktop
 * remembers the space of its rail. One instance per process, so every screen sees a change at once.
 */
class SelectedSpace private constructor(context: Context) {
  private val prefs = context.getSharedPreferences(FILE, Context.MODE_PRIVATE)
  private val id = MutableStateFlow(prefs.getString(KEY, null))

  /** The remembered space; it may be one the user left, [selectedSpace] then picks another. */
  val maybeId: StateFlow<String?> = id

  fun select(spaceId: String) {
    if (id.value == spaceId) return
    id.value = spaceId
    prefs.edit().putString(KEY, spaceId).apply()
  }

  companion object {
    private const val FILE = "home_sections"
    private const val KEY = "selected_space"

    @Volatile
    private var shared: SelectedSpace? = null

    fun of(context: Context): SelectedSpace =
      shared ?: synchronized(this) { shared ?: SelectedSpace(context.applicationContext).also { shared = it } }
  }
}
