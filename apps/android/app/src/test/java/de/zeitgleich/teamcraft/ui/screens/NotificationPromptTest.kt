package de.zeitgleich.teamcraft.ui.screens

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

private const val ANDROID_13 = 33
private const val ANDROID_12L = 32

class NotificationPromptTest {
  @Test
  fun offeredOnAndroid13WithoutThePermission() {
    assertTrue(shouldOfferNotificationPermission(ANDROID_13, hasPermission = false, hasAnswered = false))
  }

  @Test
  fun notOfferedOnceAnsweredOrGranted() {
    assertFalse(shouldOfferNotificationPermission(ANDROID_13, hasPermission = false, hasAnswered = true))
    assertFalse(shouldOfferNotificationPermission(ANDROID_13, hasPermission = true, hasAnswered = false))
  }

  @Test
  fun notOfferedBeforeAndroid13() {
    // No runtime permission there: notifications are on unless the user switched them off.
    assertFalse(shouldOfferNotificationPermission(ANDROID_12L, hasPermission = false, hasAnswered = false))
  }
}
