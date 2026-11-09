package de.zeitgleich.teamcraft.ui.theme

import android.content.Context
import android.os.Build
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import de.zeitgleich.teamcraft.settings.LocalSettingChanges

/** How the main screen lists conversations (#1151). */
enum class ChatListLayout {
  /** Home for the DMs, a tab for the channels by space. */
  Spaces,

  /** The DMs and the channels of all spaces in one list, on Home. */
  List,
}

/** How the list layout orders its channels (#1345). */
enum class ChatListGrouping {
  /** DMs and channels together, the most recent first. */
  Mixed,

  /** The DMs, then the channels of each space under its name. */
  BySpace,
}

/**
 * "Use the colors of the wallpaper" (Material You, Android 12+) instead of the TeamCraft colors.
 * A device setting, off by default so the app looks like desktop and web; a colour theme other
 * than the server's default takes precedence. The layout of the conversations, the colour theme and
 * the font follow the account (#945, #202, #1494).
 */
class AppearancePrefs private constructor(context: Context) {
  private val prefs = context.getSharedPreferences(FILE, Context.MODE_PRIVATE)
  private val dynamic = shared ?: MutableStateFlow(prefs.getBoolean(KEY_DYNAMIC, false)).also { shared = it }

  private val layout = sharedLayout ?: MutableStateFlow(
    ChatListLayout.entries.firstOrNull { it.name == prefs.getString(KEY_LAYOUT, null) } ?: ChatListLayout.Spaces,
  ).also { sharedLayout = it }

  private val grouping = sharedGrouping ?: MutableStateFlow(
    ChatListGrouping.entries.firstOrNull { it.name == prefs.getString(KEY_GROUPING, null) } ?: ChatListGrouping.Mixed,
  ).also { sharedGrouping = it }

  private val colorTheme = sharedColorTheme ?: MutableStateFlow(
    prefs.getString(KEY_COLOR_THEME, null) ?: DEFAULT_COLOR_THEME,
  ).also { sharedColorTheme = it }

  /**
   * The colour theme (#202) as the core stores it (`brand`, `preset:…`, `custom:…`); the core turns
   * it into colours (`colorThemeTokens`). Follows the account; light or dark stays the device's.
   */
  val colorThemeValue: StateFlow<String> = colorTheme

  fun setColorTheme(value: String) {
    if (value == colorTheme.value) return
    prefs.edit().putString(KEY_COLOR_THEME, value).apply()
    colorTheme.value = value
    LocalSettingChanges.changed()
  }

  private val font = sharedFont ?: MutableStateFlow(
    prefs.getString(KEY_FONT, null) ?: DEFAULT_FONT,
  ).also { sharedFont = it }

  /**
   * The interface font (#1494) by its wire name (`system`, `inter`, …, `AppFonts.kt`). Follows the
   * account; a name this app doesn't ship shows the system font.
   */
  val fontValue: StateFlow<String> = font

  fun setFont(value: String) {
    if (value == font.value) return
    prefs.edit().putString(KEY_FONT, value).apply()
    font.value = value
    LocalSettingChanges.changed()
  }

  /** Live, like [chatListLayout]. */
  val chatListGrouping: StateFlow<ChatListGrouping> = grouping

  fun setChatListGrouping(choice: ChatListGrouping) {
    prefs.edit().putString(KEY_GROUPING, choice.name).apply()
    grouping.value = choice
    LocalSettingChanges.changed()
  }

  /** Live, so the main screen switches as soon as the setting changes. */
  val chatListLayout: StateFlow<ChatListLayout> = layout

  fun setChatListLayout(choice: ChatListLayout) {
    prefs.edit().putString(KEY_LAYOUT, choice.name).apply()
    layout.value = choice
    LocalSettingChanges.changed()
  }

  /** Live, so the theme switches as soon as the setting changes. */
  val isDynamicColor: StateFlow<Boolean> = dynamic

  fun setDynamicColor(isOn: Boolean) {
    prefs.edit().putBoolean(KEY_DYNAMIC, isOn).apply()
    dynamic.value = isOn
  }

  companion object {
    private const val FILE = "appearance_prefs"
    private const val KEY_DYNAMIC = "dynamic_color"
    private const val KEY_LAYOUT = "chat_list_layout"
    private const val KEY_GROUPING = "chat_list_grouping"
    private const val KEY_COLOR_THEME = "color_theme"
    private const val KEY_FONT = "font"

    /** The server's brand colours, or the app's own without a brand. */
    const val DEFAULT_COLOR_THEME = "brand"

    /** The system's interface font. */
    const val DEFAULT_FONT = "system"

    /** One flow per process: every screen and the theme see the same value. */
    private var shared: MutableStateFlow<Boolean>? = null
    private var sharedLayout: MutableStateFlow<ChatListLayout>? = null
    private var sharedGrouping: MutableStateFlow<ChatListGrouping>? = null
    private var sharedColorTheme: MutableStateFlow<String>? = null
    private var sharedFont: MutableStateFlow<String>? = null

    val isSupported: Boolean get() = Build.VERSION.SDK_INT >= Build.VERSION_CODES.S

    fun prefs(context: Context) = AppearancePrefs(context.applicationContext)
  }
}
