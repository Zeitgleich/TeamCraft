package de.zeitgleich.teamcraft.ui

import android.media.AudioDeviceInfo
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class ChatShortcutsTest {
  private val order = listOf("a" to 0, "b" to 2, "c" to 0, "d" to 1)

  @Test
  fun nextUnreadFollowsTheCurrentConversationAndWraps() {
    assertEquals("b", nextUnreadAfter(order, "a"))
    assertEquals("d", nextUnreadAfter(order, "b"))
    assertEquals("b", nextUnreadAfter(order, "d"))
  }

  @Test
  fun nextUnreadStartsAtTheTopWhenNoneIsOpen() {
    assertEquals("b", nextUnreadAfter(order, null))
  }

  @Test
  fun nextUnreadIsNullWithoutOtherUnread() {
    assertNull(nextUnreadAfter(listOf("a" to 3), "a"))
    assertNull(nextUnreadAfter(emptyList(), null))
  }

  @Test
  fun theEarpieceIsUsedOnlyNearTheEarWithoutConnectedOutput() {
    assertTrue(shouldUseEarpiece(isNear = true, hasExternalOutput = false))
    assertFalse(shouldUseEarpiece(isNear = true, hasExternalOutput = true))
    assertFalse(shouldUseEarpiece(isNear = false, hasExternalOutput = false))
  }

  @Test
  fun aReadingBelowTheRangeIsNear() {
    assertTrue(isNearReading(0f, 5f))
    assertFalse(isNearReading(5f, 5f))
    assertFalse(isNearReading(8f, 8f))
  }

  @Test
  fun headsetsAreExternalButTheSpeakerAndEarpieceAreNot() {
    assertTrue(isExternalOutput(AudioDeviceInfo.TYPE_BLUETOOTH_A2DP))
    assertTrue(isExternalOutput(AudioDeviceInfo.TYPE_WIRED_HEADSET))
    assertFalse(isExternalOutput(AudioDeviceInfo.TYPE_BUILTIN_SPEAKER))
    assertFalse(isExternalOutput(AudioDeviceInfo.TYPE_BUILTIN_EARPIECE))
  }
}
