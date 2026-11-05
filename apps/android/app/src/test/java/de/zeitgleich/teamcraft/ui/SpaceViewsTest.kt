package de.zeitgleich.teamcraft.ui

import de.zeitgleich.teamcraft.core.ChatAnswer
import de.zeitgleich.teamcraft.core.Meeting
import de.zeitgleich.teamcraft.core.MeetingList
import de.zeitgleich.teamcraft.core.SpaceMember
import de.zeitgleich.teamcraft.core.SpaceRoster
import de.zeitgleich.teamcraft.core.WireJson
import de.zeitgleich.teamcraft.ui.screens.meetingsOfSpace
import de.zeitgleich.teamcraft.ui.screens.peopleOfRoster
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class SpaceViewsTest {
  @Test
  fun aSpaceListsItsOwnUpcomingMeetingsSoonestFirst() {
    val meetings = listOf(
      meeting("1", startsAt = 200, space = "9"),
      meeting("2", startsAt = 100, space = "9"),
      meeting("3", startsAt = 50, space = "8"),
      meeting("4", startsAt = 60, space = null),
      meeting("5", startsAt = 70, space = "9", status = "cancelled"),
    )
    assertEquals(listOf("2", "1"), meetingsOfSpace(meetings, "9").map { it.id })
  }

  @Test
  fun bridgeAnswersOfTheSpaceQueriesDecode() {
    val meetings = WireJson.decodeFromString(
      MeetingList.serializer(),
      """{"meetings":[{"id":"1","title":"Sync","starts_at":1,"ends_at":2,"conversation_id":"3","space_id":"9"},
        {"id":"2","title":"Old","starts_at":1,"ends_at":2,"conversation_id":"4"}]}""",
    ).meetings
    assertEquals("9", meetings[0].maybeSpaceId)
    assertNull(meetings[1].maybeSpaceId)
    val roster = WireJson.decodeFromString(
      ChatAnswer.serializer(),
      """{"answer":"space_members","roster":{"state":"loaded","members":[
        {"user":"5","display_name":"Ada","is_guest":true,"role":"member"}]}}""",
    ).roster
    assertEquals(SpaceMember("5", "Ada", isGuest = true), roster?.members?.single())
    val hidden = WireJson.decodeFromString(ChatAnswer.serializer(), """{"answer":"space_members","roster":{"state":"hidden"}}""")
    assertEquals(SpaceRoster("hidden"), hidden.roster)
    val calls = WireJson.decodeFromString(
      ChatAnswer.serializer(),
      """{"answer":"call_history","calls":[{"id":"7","conversation":"3","direction":"missed","started_at":5,"participants":[]}]}""",
    ).calls
    assertEquals("missed", calls.single().direction)
  }

  @Test
  fun thePeopleOfASpaceAreByNameWithoutBots() {
    val roster = SpaceRoster(
      "loaded",
      listOf(SpaceMember("1", "bea"), SpaceMember("2", "Ada"), SpaceMember("3", "Bot", isBot = true)),
    )
    assertEquals(listOf("Ada", "bea"), peopleOfRoster(roster).map { it.displayName })
  }

  private fun meeting(id: String, startsAt: Long, space: String?, status: String = "scheduled") =
    Meeting(id, "Sync", startsAt, startsAt + 1, status, "c$id", maybeSpaceId = space)
}
