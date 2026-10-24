package de.zeitgleich.teamcraft.notifications

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.teamcraft_ffi.NotificationContent

class NotificationPrivacyTest {
  @Test
  fun theAppLockShowsTheSenderOnlyWithoutActionsOrShortcut() {
    val locked = MessagePrivacy.of(NotificationContent.CONTENT, isAppLockOn = true, isVerified = true)
    assertEquals(NotificationContent.SENDER_ONLY, locked.content)
    assertTrue(locked.showsNames)
    assertFalse(locked.showsContent)
    assertFalse(locked.offersActions)
    assertFalse(locked.pushesShortcut)
    // The app lock never shows more than the setting.
    val nothing = MessagePrivacy.of(NotificationContent.NOTHING, isAppLockOn = true, isVerified = true)
    assertEquals(NotificationContent.NOTHING, nothing.content)
  }

  @Test
  fun byDefaultTheNotificationShowsEverythingAndActsWhenVerified() {
    val verified = MessagePrivacy.of(NotificationContent.CONTENT, isAppLockOn = false, isVerified = true)
    assertTrue(verified.showsContent && verified.offersActions && verified.pushesShortcut)
    val unverified = MessagePrivacy.of(NotificationContent.CONTENT, isAppLockOn = false, isVerified = false)
    assertTrue(unverified.showsContent)
    assertFalse(unverified.offersActions)
  }

  @Test
  fun senderOnlyKeepsActionsAndNothingDropsThem() {
    val sender = MessagePrivacy.of(NotificationContent.SENDER_ONLY, isAppLockOn = false, isVerified = true)
    assertFalse(sender.showsContent)
    assertTrue(sender.showsNames && sender.offersActions && sender.pushesShortcut)
    val nothing = MessagePrivacy.of(NotificationContent.NOTHING, isAppLockOn = false, isVerified = true)
    assertFalse(nothing.showsNames || nothing.offersActions || nothing.pushesShortcut)
  }

  @Test
  fun theSummaryListsNamesOnlyWhenNamesShow() {
    val conversations = listOf(
      ConversationNotification(conversationId = "1", maybeTitle = "Project A", lines = listOf(line("Alice"))),
      ConversationNotification(conversationId = "2", lines = listOf(line("Bob"))),
    )
    val locked = MessagePrivacy.of(NotificationContent.CONTENT, isAppLockOn = true, isVerified = false)
    assertEquals(listOf("Project A", "Bob"), MessagePrivacy.summaryLines(locked, conversations))
    val nothing = MessagePrivacy.of(NotificationContent.NOTHING, isAppLockOn = false, isVerified = false)
    assertEquals(emptyList<String>(), MessagePrivacy.summaryLines(nothing, conversations))
  }

  private fun line(sender: String) = NotificationLine(
    maybeMessageId = "100",
    maybeSenderKey = "user-$sender",
    senderName = sender,
    text = "secret text",
    sentAtMs = 1,
  )
}
