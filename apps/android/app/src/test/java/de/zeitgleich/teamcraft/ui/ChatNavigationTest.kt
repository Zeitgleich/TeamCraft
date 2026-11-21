package de.zeitgleich.teamcraft.ui

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

private val A = ChatScreen("a", isThread = false)
private val A_THREAD = ChatScreen("a", isThread = true)
private val B = ChatScreen("b", isThread = false)
private val B_THREAD = ChatScreen("b", isThread = true)

class ChatNavigationTest {
  @Test
  fun fromOutsideEverythingAboveTheListGoes() {
    val options = openOptions(OpenOrigin.Outside)
    assertEquals(HOME_ROUTE, options.popUpToRoute)
    assertFalse(options.isPopUpToInclusive())
    assertTrue(options.shouldLaunchSingleTop())
  }

  @Test
  fun inTheAppNothingIsPoppedUpFront() {
    val options = openOptions(OpenOrigin.InApp)
    assertNull(options.popUpToRoute)
    assertFalse(options.shouldLaunchSingleTop())
  }

  @Test
  fun aConversationFromTheListStacksOnIt() {
    assertEquals(0, chatScreensToPop(listOf(null), B))
  }

  @Test
  fun aConversationReplacesTheOpenOne() {
    // [list, A] -> [list, B]: Back from B leads to the list.
    assertEquals(1, chatScreensToPop(listOf(null, A), B))
  }

  @Test
  fun aConversationReplacesTheOpenOneAndWhatIsAboveIt() {
    // [list, A, thread of A, files] -> [list, B].
    assertEquals(3, chatScreensToPop(listOf(null, A, A_THREAD, null), B))
  }

  @Test
  fun screensBelowTheFirstChatStay() {
    // [list, search, A] -> [list, search, B]: Back leads to the search.
    assertEquals(1, chatScreensToPop(listOf(null, null, A), B))
  }

  @Test
  fun theSameConversationAgainReplacesItself() {
    assertEquals(2, chatScreensToPop(listOf(null, A, A_THREAD), A))
  }

  @Test
  fun aThreadStacksOnItsOwnConversation() {
    assertEquals(0, chatScreensToPop(listOf(null, A), A_THREAD))
  }

  @Test
  fun aThreadOfAnotherConversationReplacesTheOpenChats() {
    assertEquals(2, chatScreensToPop(listOf(null, A, A_THREAD), B_THREAD))
  }
}
