package de.zeitgleich.teamcraft.ui

import android.content.Context
import de.zeitgleich.teamcraft.settings.LocalSettingChanges

/**
 * Composer preferences. "Send with Enter" suits hardware keyboards and tablets; off by default,
 * so Enter makes a new line as in most messengers on phones. It stays on this device: a desktop's
 * Enter-sends would break new lines on a touch keyboard.
 */
class ComposerPrefs private constructor(context: Context) {
  private val prefs = context.getSharedPreferences(FILE, Context.MODE_PRIVATE)

  val isSendWithEnter: Boolean get() = prefs.getBoolean(KEY_SEND_WITH_ENTER, false)

  /** Whether others see "… is typing" while the user writes; on by default, as on desktop. Follows the account (#945). */
  val isSharingTyping: Boolean get() = prefs.getBoolean(KEY_SHARE_TYPING, true)

  fun setSharingTyping(isOn: Boolean) {
    prefs.edit().putBoolean(KEY_SHARE_TYPING, isOn).apply()
    LocalSettingChanges.changed()
  }

  /** Whether the formatting toolbar shows above the field (#1051); off until the user turns it on. */
  val isFormatBarOpen: Boolean get() = prefs.getBoolean(KEY_FORMAT_BAR, false)

  fun setFormatBarOpen(isOpen: Boolean) {
    prefs.edit().putBoolean(KEY_FORMAT_BAR, isOpen).apply()
  }

  /** Usernames the user mentioned lately, newest first: they rank first in suggestions (#2052). */
  val recentMentions: List<String>
    get() = prefs.getString(KEY_RECENT_MENTIONS, "").orEmpty().split('\n').filter { it.isNotEmpty() }

  fun noteMention(username: String) {
    val next = (listOf(username) + recentMentions.filter { !it.equals(username, ignoreCase = true) }).take(MAX_RECENT_MENTIONS)
    prefs.edit().putString(KEY_RECENT_MENTIONS, next.joinToString("\n")).apply()
  }

  /** The attach option used last (#2062): the attach menu lists it first. */
  var lastAttach: AttachKind
    get() = AttachKind.parse(prefs.getString(KEY_LAST_ATTACH, null))
    set(kind) = prefs.edit().putString(KEY_LAST_ATTACH, kind.name).apply()

  fun setSendWithEnter(isOn: Boolean) {
    prefs.edit().putBoolean(KEY_SEND_WITH_ENTER, isOn).apply()
  }

  companion object {
    private const val FILE = "composer_prefs"
    private const val KEY_SEND_WITH_ENTER = "send_with_enter"
    private const val KEY_SHARE_TYPING = "share_typing"
    private const val KEY_FORMAT_BAR = "format_bar"
    private const val KEY_LAST_ATTACH = "last_attach"
    private const val KEY_RECENT_MENTIONS = "recent_mentions"
    private const val MAX_RECENT_MENTIONS = 20

    fun prefs(context: Context) = ComposerPrefs(context.applicationContext)
  }
}

/** The ways to attach something, in the order the attach menu offers them by default. */
enum class AttachKind {
  CAMERA,
  GALLERY,
  FILE,
  ;

  companion object {
    fun parse(name: String?): AttachKind = entries.firstOrNull { it.name == name } ?: CAMERA

    /** All kinds with [last] first; the others keep their order. */
    fun ordered(last: AttachKind): List<AttachKind> = listOf(last) + entries.filter { it != last }
  }
}
