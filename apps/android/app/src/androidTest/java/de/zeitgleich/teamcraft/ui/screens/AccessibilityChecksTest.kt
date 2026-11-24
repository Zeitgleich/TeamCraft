package de.zeitgleich.teamcraft.ui.screens

import android.content.res.Configuration
import androidx.activity.compose.setContent
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.size
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.test.junit4.accessibility.enableAccessibilityChecks
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.test.tryPerformAccessibilityChecks
import androidx.compose.ui.unit.dp
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import de.zeitgleich.teamcraft.ui.ChatListEntry
import de.zeitgleich.teamcraft.ui.ChatPreview
import de.zeitgleich.teamcraft.ui.ChatTab
import de.zeitgleich.teamcraft.ui.HomeSpace
import de.zeitgleich.teamcraft.ui.MainActivity
import de.zeitgleich.teamcraft.ui.PreviewContent
import de.zeitgleich.teamcraft.ui.PreviewSender
import de.zeitgleich.teamcraft.ui.homeSpaces
import de.zeitgleich.teamcraft.ui.theme.ChatListGrouping
import de.zeitgleich.teamcraft.ui.theme.ChatListLayout
import de.zeitgleich.teamcraft.ui.theme.TeamCraftTheme
import uniffi.teamcraft_ffi.ConversationItem
import uniffi.teamcraft_ffi.ConversationKind
import uniffi.teamcraft_ffi.Presence

/**
 * The Accessibility Test Framework over the main screen (#2287), in light and dark: touch target
 * size, contrast, labels, duplicate clickable bounds. The checks throw on an ERROR result, so the
 * test fails on the first regression; warnings are not gated. More screens join as they can be
 * composed without a server (docs/accessibility.md, "Keeping it from regressing").
 *
 * Instrumented: `apps/android/scripts/docker-build.sh --emulator` (CI: android.yml).
 */
@RunWith(AndroidJUnit4::class)
class AccessibilityChecksTest {
  @get:Rule
  val rule = createAndroidComposeRule<MainActivity>()

  @Test
  fun homeInLight() = checkMainScreen(isDark = false, tab = ChatTab.Home)

  @Test
  fun homeInDark() = checkMainScreen(isDark = true, tab = ChatTab.Home)

  @Test
  fun directMessagesInLight() = checkMainScreen(isDark = false, tab = ChatTab.Direct)

  @Test
  fun directMessagesInDark() = checkMainScreen(isDark = true, tab = ChatTab.Direct)

  /** Proves the gate fires: a tiny, unlabelled click target must fail the checks. */
  @Test
  fun anUnlabelledTinyButtonFailsTheChecks() {
    rule.enableAccessibilityChecks()
    rule.activity.setContent {
      Box(Modifier.size(12.dp).clickable {})
    }
    rule.waitForIdle()
    val result = runCatching { rule.onRoot().tryPerformAccessibilityChecks() }
    assertTrue("the checks accepted a 12 dp unlabelled button", result.isFailure)
  }

  private fun checkMainScreen(isDark: Boolean, tab: ChatTab) {
    rule.enableAccessibilityChecks()
    rule.activity.setContent {
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
            layout = ChatListLayout.Spaces,
            grouping = ChatListGrouping.Mixed,
            spaces = homeSpaces(SPACES, CONVERSATIONS),
            maybeRememberedSpaceId = null,
            tab = tab,
            onTab = {},
            maybeConversations = CONVERSATIONS,
            list = ListState(
              previews = previews(),
              maybeSelectedId = null,
              selection = emptyList(),
              actionsFor = { _, _ -> RowActions(isUnread = false, toggleRead = null, toggleFavorite = {}, mute = {}) },
              onOpen = {},
              onLongPress = {},
            ),
            maybeSelectionBar = null,
            quickAccess = QuickAccess(draftCount = 1, maybeScheduledCount = 2),
            onAccount = {},
            onNavigate = {},
          )
        }
      }
    }
    rule.waitForIdle()
    rule.onRoot().tryPerformAccessibilityChecks()
  }

  private fun previews(): Map<String, ChatListEntry> {
    val now = System.currentTimeMillis()
    return mapOf(
      "1" to ChatListEntry(ChatPreview(PreviewSender.None, PreviewContent.Text("Can you review the draft?"), now), "m"),
      "4" to ChatListEntry(ChatPreview(PreviewSender.Other("Jo"), PreviewContent.Text("Release notes are up"), now), "m"),
    )
  }

  private companion object {
    val SPACES = listOf(HomeSpace("s-Project A", "Project A"), HomeSpace("s-Project B", "Project B"))

    val CONVERSATIONS = listOf(
      item("1", "Sam Doe", ConversationKind.DIRECT, unread = 2u, presence = Presence.ONLINE),
      item("2", "Kim Lee", ConversationKind.DIRECT, presence = Presence.AWAY),
      item("3", "Jo, Kim, Sam", ConversationKind.GROUP, isFavorite = true),
      item("4", "general", ConversationKind.PUBLIC, space = "Project A", unread = 4u),
      item("5", "builds", ConversationKind.PRIVATE, space = "Project A", unread = 1u, mentions = 1u),
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
