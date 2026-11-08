package de.zeitgleich.teamcraft.ui.screens

import android.content.res.Configuration
import android.graphics.Bitmap
import androidx.activity.compose.setContent
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.platform.LocalConfiguration
import androidx.test.core.app.ActivityScenario
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import java.io.File
import org.junit.Assume.assumeTrue
import org.junit.Test
import org.junit.runner.RunWith
import de.zeitgleich.teamcraft.ui.ChatListEntry
import de.zeitgleich.teamcraft.ui.ChatPreview
import de.zeitgleich.teamcraft.ui.ChatTab
import de.zeitgleich.teamcraft.ui.HomeSpace
import de.zeitgleich.teamcraft.ui.MainActivity
import de.zeitgleich.teamcraft.ui.homeSpaces
import de.zeitgleich.teamcraft.ui.theme.ChatListGrouping
import de.zeitgleich.teamcraft.ui.theme.ChatListLayout
import de.zeitgleich.teamcraft.ui.PreviewContent
import de.zeitgleich.teamcraft.ui.PreviewSender
import de.zeitgleich.teamcraft.ui.theme.TeamCraftTheme
import uniffi.teamcraft_ffi.ConversationItem
import uniffi.teamcraft_ffi.ConversationKind
import uniffi.teamcraft_ffi.Presence

private const val HOUR_MILLIS = 3_600_000L

/**
 * Screenshots of the main screen (#262) in light and dark, empty and filled, with placeholder data.
 * Not a check: it only runs when asked for (`-e screenshots true`) and writes PNGs to the app's
 * external files directory (`screenshots/`), to be pulled with adb.
 */
@RunWith(AndroidJUnit4::class)
class MainScreenScreenshots {
  @Test
  fun homeEmptyAndFilledInLightAndDark() {
    assumeTrue(InstrumentationRegistry.getArguments().getString("screenshots") == "true")
    for (isDark in listOf(false, true)) {
      val theme = if (isDark) "dark" else "light"
      shoot("home-empty-$theme", isDark, emptyList(), emptyMap(), ChatTab.Home)
      shoot("home-filled-$theme", isDark, CONVERSATIONS, previews(), ChatTab.Home)
      shoot("activity-$theme", isDark, CONVERSATIONS, previews(), ChatTab.Activity)
      shoot("direct-$theme", isDark, CONVERSATIONS, previews(), ChatTab.Direct)
      // #1526: one space at a time, a space without channels, its switcher, and the list by space.
      shoot("space-home-$theme", isDark, CONVERSATIONS, previews(), ChatTab.Home, maybeSpace = "s-Project B")
      shoot("space-empty-$theme", isDark, CONVERSATIONS, previews(), ChatTab.Home, maybeSpace = "s-Project C")
      shoot("space-switcher-$theme", isDark, CONVERSATIONS, previews(), ChatTab.Home, maybeSpace = "s-Project B", isSwitching = true)
      shoot(
        "list-by-space-$theme",
        isDark,
        CONVERSATIONS,
        previews(),
        ChatTab.Home,
        layout = ChatListLayout.List,
        grouping = ChatListGrouping.BySpace,
      )
    }
  }

  private fun shoot(
    name: String,
    isDark: Boolean,
    conversations: List<ConversationItem>,
    previews: Map<String, ChatListEntry>,
    tab: ChatTab,
    layout: ChatListLayout = ChatListLayout.Spaces,
    grouping: ChatListGrouping = ChatListGrouping.Mixed,
    maybeSpace: String? = null,
    isSwitching: Boolean = false,
  ) {
    val instrumentation = InstrumentationRegistry.getInstrumentation()
    ActivityScenario.launch(MainActivity::class.java).use { scenario ->
      scenario.onActivity { activity ->
        activity.setContent {
          val configuration = Configuration(LocalConfiguration.current).apply {
            uiMode = (uiMode and Configuration.UI_MODE_NIGHT_MASK.inv()) or
              (if (isDark) Configuration.UI_MODE_NIGHT_YES else Configuration.UI_MODE_NIGHT_NO)
          }
          CompositionLocalProvider(LocalConfiguration provides configuration) {
            TeamCraftTheme {
              MainScaffold(
                brand = MainBrand("Brand A", maybeLogo = null),
                accountUserId = "",
                accountName = "Alex Example",
                layout = layout,
                grouping = grouping,
                spaces = homeSpaces(SPACES, conversations),
                maybeRememberedSpaceId = maybeSpace,
                tab = tab,
                onTab = {},
                maybeConversations = conversations,
                list = ListState(
                  previews = previews,
                  maybeSelectedId = null,
                  selection = emptyList(),
                  actionsFor = { _, _ -> RowActions(isUnread = false, toggleRead = null, toggleFavorite = {}) },
                  onOpen = {},
                  onLongPress = {},
                ),
                maybeSelectionBar = null,
                quickAccess = QuickAccess(draftCount = 1, maybeScheduledCount = 2),
                onSearch = {},
                onAccount = {},
                onNavigate = {},
              )
              if (isSwitching) {
                val spaces = homeSpaces(SPACES, conversations)
                SpaceSwitcherSheet(spaces, maybeSpace, canBrowse = true, onSelect = {}, onBrowse = {}, onDismiss = {})
              }
            }
          }
        }
      }
      instrumentation.waitForIdleSync()
      // Not timing a result: lets the first frames (fonts, ripples) settle before the picture.
      Thread.sleep(1_500)
      val bitmap = instrumentation.uiAutomation.takeScreenshot()
      val directory = File(instrumentation.targetContext.getExternalFilesDir(null), "screenshots").apply { mkdirs() }
      File(directory, "$name.png").outputStream().use { bitmap.compress(Bitmap.CompressFormat.PNG, 100, it) }
    }
  }

  private fun previews(): Map<String, ChatListEntry> {
    val now = System.currentTimeMillis()
    fun entry(text: String, sender: PreviewSender, ageHours: Long) =
      ChatListEntry(ChatPreview(sender, PreviewContent.Text(text), now - ageHours * HOUR_MILLIS), "m")
    return mapOf(
      "1" to entry("Can you review the draft before noon?", PreviewSender.None, 0),
      "2" to entry("Thanks, see you tomorrow", PreviewSender.Me, 3),
      "3" to entry("I moved the meeting to 3 pm", PreviewSender.Other("Sam"), 26),
      "4" to entry("Release notes are up", PreviewSender.Other("Jo"), 1),
      "5" to entry("@Alex can you check the build?", PreviewSender.Other("Kim"), 2),
      "6" to entry("Lunch?", PreviewSender.None, 50),
    )
  }

  private companion object {
    val SPACES = listOf(HomeSpace("s-Project A", "Project A"), HomeSpace("s-Project B", "Project B"), HomeSpace("s-Project C", "Project C"))

    val CONVERSATIONS = listOf(
      item("1", "Sam Doe", ConversationKind.DIRECT, unread = 2u, presence = Presence.ONLINE),
      item("2", "Kim Lee", ConversationKind.DIRECT, presence = Presence.AWAY),
      item("3", "Jo, Kim, Sam", ConversationKind.GROUP, isFavorite = true),
      item("4", "general", ConversationKind.PUBLIC, space = "Project A", unread = 4u),
      item("5", "builds", ConversationKind.PRIVATE, space = "Project A", unread = 1u, mentions = 1u),
      item("6", "Jo Roe", ConversationKind.DIRECT, presence = Presence.OFFLINE),
      item("7", "design", ConversationKind.PUBLIC, space = "Project B"),
    )

    fun item(
      id: String,
      name: String,
      kind: ConversationKind,
      space: String? = null,
      isFavorite: Boolean = false,
      unread: UInt = 0u,
      mentions: UInt = 0u,
      presence: Presence? = null,
    ) = ConversationItem(
      id = id,
      maybeSpaceId = space?.let { "s-$it" },
      maybeSpaceName = space,
      kind = kind,
      name = name,
      topic = "",
      unreadCount = unread,
      mentionCount = mentions,
      isFavorite = isFavorite,
      isMuted = false,
      canPost = true,
      maybePeerPresence = presence,
    )
  }
}
