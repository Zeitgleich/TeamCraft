package de.zeitgleich.teamcraft.ui

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.teamcraft_ffi.ConversationItem
import uniffi.teamcraft_ffi.ConversationKind
import uniffi.teamcraft_ffi.LeaveChoice

class EntryMenuTest {
  @Test
  fun selectIsAlwaysTheFirstEntry() {
    for (kind in ConversationKind.values()) {
      for (leave in LeaveChoice.values()) {
        assertEquals(EntryAction.Select, conversationMenu(item(kind), leave, hasLink = true).first().action)
      }
    }
  }

  @Test
  fun aChannelMemberCanLeave() {
    val menu = conversationMenu(item(ConversationKind.PUBLIC), LeaveChoice.ALLOWED, hasLink = true)
    assertEquals(
      listOf(EntryAction.Select, EntryAction.Notifications, EntryAction.CopyLink, EntryAction.LeaveChannel),
      menu.map { it.action },
    )
    assertTrue(menu.all { it.isEnabled })
  }

  @Test
  fun aDirectMessageHasNoLeave() {
    val menu = conversationMenu(item(ConversationKind.DIRECT), LeaveChoice.HIDDEN, hasLink = true)
    assertFalse(menu.any { it.action == EntryAction.LeaveChannel })
  }

  @Test
  fun theLastAdminSeesLeaveDisabled() {
    val leave = conversationMenu(item(ConversationKind.PRIVATE), LeaveChoice.LAST_ADMIN, hasLink = true)
      .single { it.action == EntryAction.LeaveChannel }
    assertFalse(leave.isEnabled)
  }

  @Test
  fun markReadOnlyWhereSomethingIsUnread() {
    assertFalse(conversationMenu(item(ConversationKind.PUBLIC), LeaveChoice.ALLOWED, hasLink = false).any { it.action == EntryAction.MarkRead })
    assertTrue(conversationMenu(item(ConversationKind.PUBLIC, unread = 3u), LeaveChoice.ALLOWED, hasLink = false).any { it.action == EntryAction.MarkRead })
    assertTrue(conversationMenu(item(ConversationKind.PUBLIC, mentions = 1u), LeaveChoice.ALLOWED, hasLink = false).any { it.action == EntryAction.MarkRead })
  }

  @Test
  fun noLinkWithoutAWebAddress() {
    assertFalse(conversationMenu(item(ConversationKind.PUBLIC), LeaveChoice.ALLOWED, hasLink = false).any { it.action == EntryAction.CopyLink })
  }

  @Test
  fun aSpaceMemberOrGuestCanMuteAndLeave() {
    assertEquals(
      listOf(EntryAction.MuteSpace, EntryAction.MarkSpaceRead, EntryAction.LeaveSpace),
      spaceMenu(hasUnread = true, leave = LeaveChoice.ALLOWED).map { it.action },
    )
    assertEquals(
      listOf(EntryAction.MuteSpace, EntryAction.LeaveSpace),
      spaceMenu(hasUnread = false, leave = LeaveChoice.ALLOWED).map { it.action },
    )
  }

  @Test
  fun theLastSpaceAdminSeesLeaveDisabled() {
    val leave = spaceMenu(hasUnread = false, leave = LeaveChoice.LAST_ADMIN).single { it.action == EntryAction.LeaveSpace }
    assertFalse(leave.isEnabled)
  }

  private fun item(kind: ConversationKind, unread: UInt = 0u, mentions: UInt = 0u) = ConversationItem(
    id = "1",
    maybeSpaceId = null,
    maybeSpaceName = null,
    kind = kind,
    name = "c1",
    topic = "",
    unreadCount = unread,
    mentionCount = mentions,
    isFavorite = false,
    isMuted = false,
    canPost = true,
  )
}
