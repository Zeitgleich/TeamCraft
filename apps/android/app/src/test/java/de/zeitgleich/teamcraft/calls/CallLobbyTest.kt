package de.zeitgleich.teamcraft.calls

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.teamcraft_ffi.LobbyPerson
import uniffi.teamcraft_ffi.LobbyPersonStatus
import uniffi.teamcraft_ffi.MeetingLobby

/** The lobby of a meeting in its call, for its hosts (#1859), as desktop and web offer it. */
class CallLobbyTest {
  private fun person(id: String, status: LobbyPersonStatus) =
    LobbyPerson(userId = id, displayName = "Person $id", status = status, canAdmit = false)

  private fun lobby(vararg people: LobbyPerson) = MeetingLobby(meetingId = "7", people = people.toList())

  @Test
  fun theLobbyIsReadAgainOnlyWhileAGuestsKeyCodeIsMissing() {
    assertTrue(isWaitingForKeyCode(lobby(person("2", LobbyPersonStatus.Guest(maybeKeyCode = null)))))
    assertFalse(isWaitingForKeyCode(lobby(person("3", LobbyPersonStatus.Guest(maybeKeyCode = "12345")))))
    assertFalse("nothing to make for a member or a guest without a device", isWaitingForKeyCode(lobby(person("1", LobbyPersonStatus.Member), person("4", LobbyPersonStatus.GuestDevicePending))))
    assertFalse(isWaitingForKeyCode(lobby()))
  }
}
