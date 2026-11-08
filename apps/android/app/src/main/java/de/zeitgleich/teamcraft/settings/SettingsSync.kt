package de.zeitgleich.teamcraft.settings

import android.content.Context
import android.util.Log
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.FlowPreview
import kotlinx.coroutines.flow.debounce
import kotlinx.coroutines.flow.drop
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import de.zeitgleich.teamcraft.calls.CallSoundPrefs
import de.zeitgleich.teamcraft.core.CoreRepository
import de.zeitgleich.teamcraft.notifications.MentionPrefs
import de.zeitgleich.teamcraft.ui.AnimatedImagesMode
import de.zeitgleich.teamcraft.ui.ComposerPrefs
import de.zeitgleich.teamcraft.ui.MediaPrefs
import de.zeitgleich.teamcraft.ui.theme.AppearancePrefs
import de.zeitgleich.teamcraft.ui.theme.ChatListGrouping
import de.zeitgleich.teamcraft.ui.theme.ChatListLayout
import uniffi.teamcraft_ffi.AppAnimatedImages
import uniffi.teamcraft_ffi.AppListGrouping
import uniffi.teamcraft_ffi.AppListLayout
import uniffi.teamcraft_ffi.MentionSettings
import uniffi.teamcraft_ffi.SyncedAppSettings

/**
 * Keeps the settings that follow the account in step with it (#945, e2ee.md §6.4): the chat list's
 * layout, animated images, typing, mention rules, call sounds and the colour theme. The rules and the
 * mapping live in the core (`teamcraft_client_api::mobile_settings`); this hands over the values of
 * the app's stores and writes back what changed.
 *
 * It runs when the core opens, when the account's settings moved (another device's change arrives
 * through the gateway, no polling), and [WRITE_DEBOUNCE_MS] after the user changed one, so a row of
 * changes costs one upload. A change made offline stays in the store and goes out on the next run.
 */
class SettingsSync(
  context: Context,
  private val core: CoreRepository,
  private val mentions: MentionPrefs,
  private val scope: CoroutineScope,
) {
  private val appContext = context.applicationContext
  private val store = context.getSharedPreferences(FILE, Context.MODE_PRIVATE)
  private val lock = Mutex()
  /** The account's settings mark of the last run, per opened core; `null` makes the next look run. */
  private var lastMark: ULong? = null
  private var lastGeneration = -1L

  @OptIn(FlowPreview::class)
  fun start() {
    scope.launch {
      core.changeCount.debounce(MARK_DEBOUNCE_MS).collect { syncIfAccountMoved() }
    }
    scope.launch {
      LocalSettingChanges.changes.drop(1).debounce(WRITE_DEBOUNCE_MS).collect { sync() }
    }
  }

  private suspend fun syncIfAccountMoved() {
    val mark = runCatching { core.core().settingsMark() }.getOrNull() ?: return
    val generation = core.accountGeneration.value
    if (mark == lastMark && generation == lastGeneration) return
    sync()
  }

  private suspend fun sync() = lock.withLock {
    val handle = runCatching { core.core() }.getOrNull() ?: return@withLock
    val generation = core.accountGeneration.value
    val local = current()
    val outcome = try {
      handle.syncSettings(local, store.getString(KEY_STATE, "").orEmpty())
    } catch (error: Exception) {
      // The values wait in the stores; the next change or account update tries again.
      Log.w(TAG, "settings not synced", error)
      return@withLock
    }
    store.edit().putString(KEY_STATE, outcome.state).apply()
    if (outcome.settings != local) LocalSettingChanges.fromAccount { apply(outcome.settings, local) }
    lastMark = runCatching { handle.settingsMark() }.getOrNull()
    lastGeneration = generation
  }

  private fun current(): SyncedAppSettings {
    val appearance = AppearancePrefs.prefs(appContext)
    val mention = mentions.settings.value
    return SyncedAppSettings(
      listLayout = when (appearance.chatListLayout.value) {
        ChatListLayout.Spaces -> AppListLayout.SPACES
        ChatListLayout.List -> AppListLayout.LIST
      },
      listGrouping = when (appearance.chatListGrouping.value) {
        ChatListGrouping.Mixed -> AppListGrouping.MIXED
        ChatListGrouping.BySpace -> AppListGrouping.BY_SPACE
      },
      animatedImages = when (MediaPrefs.prefs(appContext).animatedImages.value) {
        AnimatedImagesMode.AUTOPLAY -> AppAnimatedImages.AUTOPLAY
        AnimatedImagesMode.ON_TAP -> AppAnimatedImages.ON_TAP
      },
      sharesTyping = ComposerPrefs.prefs(appContext).isSharingTyping,
      firstNameIsMention = mention.isFirstNameMention,
      channelMentionsNotify = mention.doChannelMentionsNotify,
      keywords = mention.keywords,
      confirmChannelOver = mentions.confirmChannelOver.coerceAtLeast(0).toUInt(),
      callStatusSounds = CallSoundPrefs.prefs(appContext).statusSounds,
      colorTheme = appearance.colorThemeValue.value,
    )
  }

  /** Writes only what the account changed, so each store tells its readers only when it has news. */
  private fun apply(settings: SyncedAppSettings, before: SyncedAppSettings) {
    val appearance = AppearancePrefs.prefs(appContext)
    if (settings.listLayout != before.listLayout) {
      appearance.setChatListLayout(
        when (settings.listLayout) {
          AppListLayout.SPACES -> ChatListLayout.Spaces
          AppListLayout.LIST -> ChatListLayout.List
        },
      )
    }
    if (settings.listGrouping != before.listGrouping) {
      appearance.setChatListGrouping(
        when (settings.listGrouping) {
          AppListGrouping.MIXED -> ChatListGrouping.Mixed
          AppListGrouping.BY_SPACE -> ChatListGrouping.BySpace
        },
      )
    }
    if (settings.animatedImages != before.animatedImages) {
      MediaPrefs.prefs(appContext).setAnimatedImages(
        when (settings.animatedImages) {
          AppAnimatedImages.AUTOPLAY -> AnimatedImagesMode.AUTOPLAY
          AppAnimatedImages.ON_TAP -> AnimatedImagesMode.ON_TAP
        },
      )
    }
    if (settings.sharesTyping != before.sharesTyping) ComposerPrefs.prefs(appContext).setSharingTyping(settings.sharesTyping)
    val mention = MentionSettings(
      isFirstNameMention = settings.firstNameIsMention,
      doChannelMentionsNotify = settings.channelMentionsNotify,
      keywords = settings.keywords,
    )
    if (mention != mentions.settings.value) mentions.update(mention)
    if (settings.confirmChannelOver != before.confirmChannelOver) {
      mentions.setConfirmChannelOver(settings.confirmChannelOver.coerceAtMost(Int.MAX_VALUE.toUInt()).toInt())
    }
    if (settings.callStatusSounds != before.callStatusSounds) CallSoundPrefs.prefs(appContext).setStatusSounds(settings.callStatusSounds)
    if (settings.colorTheme != before.colorTheme) appearance.setColorTheme(settings.colorTheme)
  }

  private companion object {
    const val TAG = "SettingsSync"
    const val FILE = "settings_sync"
    const val KEY_STATE = "state"
    /** The store reports a burst of changes during a catch-up; one look at the mark is enough. */
    const val MARK_DEBOUNCE_MS = 500L
    /** A row of changes in Settings goes to the account as one upload. */
    const val WRITE_DEBOUNCE_MS = 1500L
  }
}
