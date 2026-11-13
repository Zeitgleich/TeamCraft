package de.zeitgleich.teamcraft.calls

import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.ChatSnapshot
import de.zeitgleich.teamcraft.core.User
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.teamcraft_ffi.MeetingHostAction
import uniffi.teamcraft_ffi.MeetingHosts
import uniffi.teamcraft_ffi.MeetingPart

/** Co-hosts and handing a meeting over in its call (#1720), as desktop and web offer them. */
class CallHostsTest {
  private fun hosts(ownPart: MeetingPart) =
    MeetingHosts(meetingId = "7", organizerUserId = "1", coHostUserIds = listOf("2"), ownPart = ownPart)

  private fun user(id: String, isGuest: Boolean = false, isBot: Boolean = false) =
    User(id = id, username = "u$id", displayName = "User $id", isGuest = isGuest, isBot = isBot)

  @Test
  fun tilesNameTheHostAndTheCoHosts() {
    val hosts = hosts(MeetingPart.PARTICIPANT)
    assertEquals(R.string.call_tile_host, hostLabelOf(hosts, "1"))
    assertEquals(R.string.call_tile_co_host, hostLabelOf(hosts, "2"))
    assertNull(hostLabelOf(hosts, "3"))
    assertNull("a call outside a meeting has no hosts", hostLabelOf(null, "1"))
  }

  @Test
  fun theOrganizersMenuMakesOrRemovesCoHostsAndHandsOver() {
    val hosts = hosts(MeetingPart.ORGANIZER)
    assertEquals(listOf(MeetingHostAction.MAKE_CO_HOST, MeetingHostAction.HAND_OVER), hostActionsFor(hosts, user("3")))
    assertEquals(listOf(MeetingHostAction.REMOVE_CO_HOST, MeetingHostAction.HAND_OVER), hostActionsFor(hosts, user("2")))
  }

  @Test
  fun guestsBotsAndOtherPeoplesMenusGetNothing() {
    val hosts = hosts(MeetingPart.ORGANIZER)
    assertTrue("the server refuses guests as hosts", hostActionsFor(hosts, user("3", isGuest = true)).isEmpty())
    assertTrue(hostActionsFor(hosts, user("4", isBot = true)).isEmpty())
    assertTrue("only the organizer changes the hosts", hostActionsFor(hosts(MeetingPart.CO_HOST), user("3")).isEmpty())
    assertTrue(hostActionsFor(null, user("3")).isEmpty())
  }

  @Test
  fun theMeetingGoesOnlyToMembersInTheCall() {
    val snapshot = ChatSnapshot(
      me = user("1"),
      users = listOf(user("2"), user("3", isGuest = true), user("4", isBot = true), user("5")),
    )
    val candidates = handoverCandidates(listOf("1", "2", "3", "4", "2", "9"), snapshot)
    assertEquals(listOf("2"), candidates.map { it.id })
  }

  @Test
  fun theLobbyCountsItsEntries() {
    assertEquals(2, lobbyWaitingCount("""{"entries":[{"user":{}},{"user":{}}],"is_host":true}"""))
    assertEquals(0, lobbyWaitingCount("""{"entries":[]}"""))
    assertNull(lobbyWaitingCount("not json"))
    assertNull(lobbyWaitingCount("""[1,2]"""))
  }
}
