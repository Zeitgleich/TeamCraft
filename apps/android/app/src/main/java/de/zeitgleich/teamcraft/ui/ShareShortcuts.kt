package de.zeitgleich.teamcraft.ui

import android.content.Context
import android.content.Intent
import android.util.Log
import androidx.core.content.LocusIdCompat
import androidx.core.content.pm.ShortcutInfoCompat
import androidx.core.content.pm.ShortcutManagerCompat
import de.zeitgleich.teamcraft.notifications.LaunchToken
import de.zeitgleich.teamcraft.notifications.PersonIcons
import de.zeitgleich.teamcraft.security.AppLockPrefs

/**
 * Direct share (issue #829): the notes and the recent conversations as sharing shortcuts, so the
 * system share sheet offers them directly (`res/xml/shortcuts.xml`). A shortcut's ID is the
 * conversation's, the same as the notification's conversation shortcut, which carries the same
 * category, so pushing one never takes the other's place away. Signing out removes them with the
 * notification shortcuts.
 */
object ShareShortcuts {
  /** The `share-target` category of `res/xml/shortcuts.xml`. */
  const val CATEGORY = "de.zeitgleich.teamcraft.category.SHARE_TARGET"

  /** Most conversations offered; the share sheet shows only a few per app anyway. */
  const val MAX_TARGETS = 4

  private const val TAG = "ShareShortcuts"

  /** A conversation for the share sheet. */
  data class Target(val conversationId: String, val label: String)

  /**
   * Offers [targets] (the first [MAX_TARGETS]) in the share sheet. Not while the app lock is on:
   * the share sheet would show conversation names of the locked app, and a pick reaches into it.
   */
  fun publish(context: Context, targets: List<Target>) {
    if (AppLockPrefs.prefs(context).isEnabled) return
    for (target in targets.filter { it.label.isNotBlank() }.take(MAX_TARGETS)) {
      val label = target.label
      val shortcut = ShortcutInfoCompat.Builder(context, target.conversationId)
        .setShortLabel(label)
        .setLongLived(true)
        .setLocusId(LocusIdCompat(target.conversationId))
        .setCategories(setOf(CATEGORY))
        .setIcon(PersonIcons.of(target.conversationId, label))
        .setIntent(openIntent(context, target.conversationId))
        .build()
      try {
        ShortcutManagerCompat.pushDynamicShortcut(context, shortcut)
      } catch (failure: Exception) {
        // Rate-limited or refused by the launcher: the picker still offers the conversation.
        Log.w(TAG, "share shortcut not pushed: ${failure.javaClass.simpleName}")
      }
    }
  }

  /** Tells the system the user shared to this conversation, so it ranks it higher next time. */
  fun reportUsed(context: Context, conversationId: String) {
    runCatching { ShortcutManagerCompat.reportShortcutUsed(context, conversationId) }
  }

  /**
   * The conversation a share sheet shortcut picked, if any. Any app can put an ID into a share;
   * it only preselects where the composer opens, and only among the user's share targets.
   */
  fun targetOf(intent: Intent): String? =
    runCatching { intent.getStringExtra(ShortcutManagerCompat.EXTRA_SHORTCUT_ID) }
      .getOrNull()
      ?.takeIf(LaunchToken::isConversationId)

  private fun openIntent(context: Context, conversationId: String): Intent =
    Intent(context, MainActivity::class.java)
      .setAction(Intent.ACTION_VIEW)
      .putExtra(MainActivity.EXTRA_CONVERSATION_ID, conversationId)
      .putExtra(MainActivity.EXTRA_LAUNCH_TOKEN, LaunchToken.of(context))
}
