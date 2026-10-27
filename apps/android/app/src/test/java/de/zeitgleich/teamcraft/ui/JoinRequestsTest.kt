package de.zeitgleich.teamcraft.ui

import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.BrowsableChannel
import de.zeitgleich.teamcraft.core.WireJson
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import uniffi.teamcraft_ffi.JoinRequestItem
import uniffi.teamcraft_ffi.JoinRequestOutcome
import uniffi.teamcraft_ffi.JoinRequestStanding

/** Admins' requests to join a private channel on the phone (admin.md §2.4.1, issue #799). */
class JoinRequestsTest {
  @Test
  fun theBrowserOffersWhatTheCoreDecided() {
    assertEquals(BrowseJoin.Open, BrowseJoin.of("open"))
    assertEquals(BrowseJoin.AsAdmin, BrowseJoin.of("as_admin"))
    assertEquals(BrowseJoin.NeedsApproval, BrowseJoin.of("needs_approval"))
    assertEquals(BrowseJoin.Requested, BrowseJoin.of("requested"))
    assertNull("nothing to press for a way in this version doesn't know", BrowseJoin.of("by_invitation"))
  }

  @Test
  fun aChannelFromAnOlderCoreIsAnOpenOne() {
    val channel = WireJson.decodeFromString(
      BrowsableChannel.serializer(),
      """{"conversation":"7","space":"1","name":"general"}""",
    )
    assertEquals(BrowseJoin.Open, BrowseJoin.of(channel.join))
    val asked = WireJson.decodeFromString(
      BrowsableChannel.serializer(),
      """{"conversation":"8","space":"1","name":"board","join":"requested"}""",
    )
    assertEquals(BrowseJoin.Requested, BrowseJoin.of(asked.join))
  }

  @Test
  fun onlyAnActionThatChangedNothingIsWorded() {
    for (step in JoinStep.entries) assertNull(joinOutcomeMessage(step, JoinRequestOutcome.DONE))
    assertEquals(R.string.join_request_gone, joinOutcomeMessage(JoinStep.Approve, JoinRequestOutcome.GONE))
    assertEquals(R.string.join_request_gone, joinOutcomeMessage(JoinStep.Decline, JoinRequestOutcome.GONE))
    assertEquals(R.string.join_request_not_admin, joinOutcomeMessage(JoinStep.Approve, JoinRequestOutcome.NOT_AN_ADMIN))
    assertEquals(R.string.join_ask_not_admin, joinOutcomeMessage(JoinStep.Ask, JoinRequestOutcome.NOT_AN_ADMIN))
    assertEquals(R.string.join_ask_too_soon, joinOutcomeMessage(JoinStep.Ask, JoinRequestOutcome.TOO_SOON))
    assertEquals(
      R.string.join_request_not_approvable,
      joinOutcomeMessage(JoinStep.Approve, JoinRequestOutcome.NOT_APPROVABLE),
    )
  }

  @Test
  fun aChannelShowsOnlyItsOwnRequestsEachKeyedByAdminAndChannel() {
    val requests = listOf(request("7", "2"), request("8", "2"), request("7", "3"))
    assertEquals(listOf(request("7", "2"), request("7", "3")), requestsIn("7", requests))
    assertEquals(emptyList<JoinRequestItem>(), requestsIn("9", requests))
    assertEquals("7:2", JoinRequestActions.key(request("7", "2")))
  }

  private fun request(conversation: String, user: String) = JoinRequestItem(
    conversationId = conversation,
    conversationName = "planning",
    userId = user,
    userName = "Admin $user",
    maybeUsername = "admin.$user",
    userLabel = "Admin $user (@admin.$user)",
    requestedAtMs = 10,
    expiresAtMs = 20,
    standing = JoinRequestStanding.APPROVABLE,
    notificationTag = "admin-join:$conversation:$user",
  )
}
