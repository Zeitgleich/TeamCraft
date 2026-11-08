package de.zeitgleich.teamcraft.ui

import android.content.Context
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow

/**
 * The Home sections the user collapsed (#1488), remembered on this device as the desktop sidebar
 * does: keys from [collapseKey]. One instance per process, so every screen sees a change at once.
 */
class CollapsedSections private constructor(context: Context) {
  private val prefs = context.getSharedPreferences(FILE, Context.MODE_PRIVATE)
  private val keys = MutableStateFlow(prefs.getStringSet(KEY, emptySet()).orEmpty().toSet())

  val collapsed: StateFlow<Set<String>> = keys

  fun toggle(key: String) {
    val next = if (key in keys.value) keys.value - key else keys.value + key
    keys.value = next
    prefs.edit().putStringSet(KEY, next).apply()
  }

  companion object {
    private const val FILE = "home_sections"
    private const val KEY = "collapsed"

    @Volatile
    private var shared: CollapsedSections? = null

    fun of(context: Context): CollapsedSections =
      shared ?: synchronized(this) { shared ?: CollapsedSections(context.applicationContext).also { shared = it } }
  }
}
