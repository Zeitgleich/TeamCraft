package de.zeitgleich.teamcraft.notifications

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.teamcraft_ffi.JoinRequestStanding
import uniffi.teamcraft_ffi.NotificationContent

/** The notification of an admin's request to join a channel the user owns (issue #799). */
class JoinRequestNotificationsTest {
  @Test
  fun anAdminOfTheFreshRosterCanBeApprovedFromTheNotification() {
    val spec = JoinRequestNotifications.spec(JoinRequestStanding.APPROVABLE, privacy(NotificationContent.CONTENT))
    assertTrue(spec.showsNames)
    assertEquals(listOf(JoinRequestNotificationAction.DECLINE, JoinRequestNotificationAction.APPROVE), spec.actions)
  }

  @Test
  fun somebodyOnlyTheServerCallsAnAdminCanOnlyBeDeclined() {
    val spec = JoinRequestNotifications.spec(JoinRequestStanding.NOT_AN_ADMIN, privacy(NotificationContent.SENDER_ONLY))
    assertTrue(spec.showsNames)
    assertEquals(listOf(JoinRequestNotificationAction.DECLINE), spec.actions)
  }

  @Test
  fun withoutNamesOrBehindTheAppLockTheDecisionIsMadeInTheApp() {
    val hidden = JoinRequestNotifications.spec(JoinRequestStanding.APPROVABLE, privacy(NotificationContent.NOTHING))
    assertFalse(hidden.showsNames)
    assertEquals(emptyList<JoinRequestNotificationAction>(), hidden.actions)
    val locked = JoinRequestNotifications.spec(
      JoinRequestStanding.APPROVABLE,
      MessagePrivacy.of(NotificationContent.CONTENT, isAppLockOn = true, isVerified = true),
    )
    assertTrue("the app lock still names who asks", locked.showsNames)
    assertEquals(emptyList<JoinRequestNotificationAction>(), locked.actions)
  }

  private fun privacy(content: NotificationContent) = MessagePrivacy.of(content, isAppLockOn = false, isVerified = true)
}
