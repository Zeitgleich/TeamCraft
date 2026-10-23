package de.zeitgleich.teamcraft.notifications

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class NotificationPrivacyTest {
  @Test
  fun withTheAppLockOnNothingNamesTheConversationOrSender() {
    val locked = MessagePrivacy.of(isAppLockOn = true, isVerified = true)
    assertFalse(locked.showsContent)
    assertFalse(locked.offersActions)
    assertFalse(locked.pushesShortcut)
  }

  @Test
  fun withoutTheAppLockTheNotificationShowsEverythingAndActsWhenVerified() {
    val verified = MessagePrivacy.of(isAppLockOn = false, isVerified = true)
    assertTrue(verified.showsContent && verified.offersActions && verified.pushesShortcut)
    val unverified = MessagePrivacy.of(isAppLockOn = false, isVerified = false)
    assertTrue(unverified.showsContent)
    assertFalse(unverified.offersActions)
  }

  @Test
  fun theSummaryListsNamesOnlyWithoutTheAppLock() {
    val conversations = listOf(
      ConversationNotification(conversationId = "1", maybeTitle = "Project A", lines = listOf(line("Alice"))),
      ConversationNotification(conversationId = "2", lines = listOf(line("Bob"))),
    )
    assertEquals(listOf("Project A", "Bob"), MessagePrivacy.summaryLines(isAppLockOn = false, conversations))
    assertEquals(emptyList<String>(), MessagePrivacy.summaryLines(isAppLockOn = true, conversations))
  }

  private fun line(sender: String) = NotificationLine(
    maybeMessageId = "100",
    maybeSenderKey = "user-$sender",
    senderName = sender,
    text = "secret text",
    sentAtMs = 1,
  )
}
