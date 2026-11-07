package de.zeitgleich.teamcraft.ui

import de.zeitgleich.teamcraft.R
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import uniffi.teamcraft_ffi.AppointerAuthorityItem
import uniffi.teamcraft_ffi.ChannelMemberAppointer
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
  fun theAppointerIsNamedOnlyWhereTheDirectoryHasThem() {
    assertEquals(
      R.string.channel_role_appointed_by_workspace_admin,
      channelRoleAppointedByText(ChannelMemberAppointer("Alice", AppointerAuthorityItem.WORKSPACE_ADMIN)),
    )
    assertEquals(
      R.string.channel_role_appointed_by_owner,
      channelRoleAppointedByText(ChannelMemberAppointer("Alice", AppointerAuthorityItem.WORKSPACE_OWNER)),
    )
    assertEquals(
      R.string.channel_role_appointed_by_channel_admin,
      channelRoleAppointedByText(ChannelMemberAppointer("Alice", AppointerAuthorityItem.CHANNEL_ADMIN)),
    )
    assertEquals(
      R.string.channel_role_appointed_by_channel_admin_unnamed,
      channelRoleAppointedByText(ChannelMemberAppointer(null, AppointerAuthorityItem.CHANNEL_ADMIN)),
    )
    val unnamed = AppointerAuthorityItem.entries.map { channelRoleAppointedByText(ChannelMemberAppointer(null, it)) }
    assertEquals(unnamed.size, unnamed.toSet().size)
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
