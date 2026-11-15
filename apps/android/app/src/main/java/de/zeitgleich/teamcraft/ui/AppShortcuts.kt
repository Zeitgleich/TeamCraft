package de.zeitgleich.teamcraft.ui

import android.content.Context
import android.content.Intent
import android.util.Log
import androidx.core.content.pm.ShortcutInfoCompat
import androidx.core.content.pm.ShortcutManagerCompat
import androidx.core.graphics.drawable.IconCompat
import de.zeitgleich.teamcraft.R
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.withContext

/** Where the launcher's app icon shortcuts lead (issue #2061). */
enum class LaunchShortcut(val id: String, val route: String) {
  NewMessage("new_message", "browse?people=true"),
  Search("search", "search"),
  ;

  companion object {
    const val EXTRA = "de.zeitgleich.teamcraft.extra.SHORTCUT"

    /** The shortcut an intent asks for; any app can send one, so only known names count. */
    fun of(intent: Intent): LaunchShortcut? = runCatching { intent.getStringExtra(EXTRA) }.getOrNull()?.let { name -> entries.firstOrNull { it.id == name } }

    /** The shortcut the signed-in navigation has not opened yet. */
    val pending = MutableStateFlow<LaunchShortcut?>(null)
  }
}

/**
 * The app icon's long-press menu: "New message" and "Search", then the most recent conversations
 * ([ShareShortcuts], #2061). Dynamic, because a static shortcut's intent can't name the package of a
 * debug build.
 */
object AppShortcuts {
  private const val TAG = "AppShortcuts"

  suspend fun publish(context: Context): Unit = withContext(Dispatchers.IO) {
    val shortcuts = listOf(
      shortcut(context, LaunchShortcut.NewMessage, R.string.shortcut_new_message, R.drawable.ic_shortcut_new_message, rank = 0),
      shortcut(context, LaunchShortcut.Search, R.string.shortcut_search, R.drawable.ic_shortcut_search, rank = 1),
    )
    try {
      for (shortcut in shortcuts) ShortcutManagerCompat.pushDynamicShortcut(context, shortcut)
    } catch (failure: Exception) {
      // Rate-limited or refused by the launcher: the menu just lacks them.
      Log.w(TAG, "app shortcuts not pushed: ${failure.javaClass.simpleName}")
    }
  }

  private fun shortcut(context: Context, target: LaunchShortcut, label: Int, icon: Int, rank: Int): ShortcutInfoCompat =
    ShortcutInfoCompat.Builder(context, "app-" + target.id)
      .setShortLabel(context.getString(label))
      .setIcon(IconCompat.createWithResource(context, icon))
      .setRank(rank)
      .setIntent(
        Intent(context, MainActivity::class.java)
          .setAction(Intent.ACTION_MAIN)
          .putExtra(LaunchShortcut.EXTRA, target.id),
      )
      .build()
}
