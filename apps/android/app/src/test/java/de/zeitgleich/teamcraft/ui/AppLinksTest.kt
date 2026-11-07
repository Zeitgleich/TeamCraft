package de.zeitgleich.teamcraft.ui

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import uniffi.teamcraft_ffi.AppLinkPlace
import uniffi.teamcraft_ffi.OpenedAppLink
import uniffi.teamcraft_ffi.OpenedAppLinkStep

class AppLinksTest {
  @Test
  fun aLinkWaitsForTheServerInUse() {
    // A cold start shows the cached list before the stored server is read: judged then, a link to
    // the signed-in server reads as one to a server without an account.
    assertNull(linkServerOf(null, isLocked = false, isKnown = false))
    assertNull(linkServerOf(ORIGIN, isLocked = false, isKnown = false))
    assertEquals(LinkServer(ORIGIN, isLocked = false), linkServerOf(ORIGIN, isLocked = false, isKnown = true))
    // Known and empty is an answer of its own: no server yet, a link may offer signing in.
    assertEquals(LinkServer(null, isLocked = true), linkServerOf(null, isLocked = true, isKnown = true))
  }

  @Test
  fun noQuestionWhileStartingOrSigningIn() {
    assertEquals(FOREIGN, dueLinkQuestion(FOREIGN, isStarting = false, isSigningInForLink = false))
    assertNull(dueLinkQuestion(FOREIGN, isStarting = true, isSigningInForLink = false))
    assertNull(dueLinkQuestion(FOREIGN, isStarting = false, isSigningInForLink = true))
  }

  @Test
  fun aLinkToTheServerInUseAsksNothing() {
    val link = FOREIGN.copy(step = OpenedAppLinkStep.Navigate)
    assertNull(dueLinkQuestion(link, isStarting = false, isSigningInForLink = false))
    assertNull(dueLinkQuestion(null, isStarting = false, isSigningInForLink = false))
  }

  private companion object {
    const val ORIGIN = "https://chat.example.com"
    val FOREIGN = OpenedAppLink(
      step = OpenedAppLinkStep.AskForeignServer("chat.example.org", "example.org", canAddAccount = false),
      place = AppLinkPlace.Conversation("1"),
      maybeWebUrl = "https://chat.example.org/c/1",
    )
  }
}
