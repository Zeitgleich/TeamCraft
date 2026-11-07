package de.zeitgleich.teamcraft.ui

import de.zeitgleich.teamcraft.R
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import uniffi.teamcraft_ffi.ChannelMemberRole
import uniffi.teamcraft_ffi.ReviewedRolesOutcome
import uniffi.teamcraft_ffi.RoleKeyMarkItem
import uniffi.teamcraft_ffi.RoleReviewRefusalItem

/** A channel's members section on the phone (issue #1262), worded as on desktop and web. */
class ChannelMembersTest {
  @Test
  fun onlyAdminsAndModeratorsGetABadge() {
    assertEquals(R.string.channel_role_admin, channelRoleLabel(ChannelMemberRole.ADMIN))
    assertEquals(R.string.channel_role_moderator, channelRoleLabel(ChannelMemberRole.MODERATOR))
    assertNull(channelRoleLabel(ChannelMemberRole.MEMBER))
    assertNull("a role this version doesn't know isn't named", channelRoleLabel(ChannelMemberRole.OTHER))
  }

  @Test
  fun aCheckedKeyIsNotMarked() {
    assertNull(roleKeyMarkLabel(RoleKeyMarkItem.CHECKED))
    assertEquals(R.string.channel_roles_review_key_changed, roleKeyMarkLabel(RoleKeyMarkItem.CHANGED))
    assertEquals(R.string.channel_roles_review_key_uncheckable, roleKeyMarkLabel(RoleKeyMarkItem.UNCHECKABLE))
  }

  @Test
  fun everyRefusalHasItsOwnText() {
    val texts = RoleReviewRefusalItem.entries.map(::roleReviewRefusalText)
    assertEquals(texts.size, texts.toSet().size)
    assertEquals(R.string.channel_roles_review_no_admin, roleReviewRefusalText(RoleReviewRefusalItem.NO_ADMIN))
  }

  @Test
  fun onlyAConfirmationThatDidNotHappenIsWorded() {
    assertNull(reviewedRolesMessage(ReviewedRolesOutcome.CONFIRMED))
    assertEquals(R.string.channel_roles_review_changed, reviewedRolesMessage(ReviewedRolesOutcome.CHANGED))
    assertEquals(R.string.error_generic, reviewedRolesMessage(ReviewedRolesOutcome.REFUSED))
  }

  @Test
  fun somebodyNobodyCanNameIsTheUnknownPerson() {
    assertEquals("Alice, Unknown person", droppedRoleNames(listOf("Alice", null), "Unknown person"))
  }
}
