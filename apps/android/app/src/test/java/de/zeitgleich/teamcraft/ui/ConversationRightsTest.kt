package de.zeitgleich.teamcraft.ui

import org.junit.Assert.assertEquals
import org.junit.Test
import uniffi.teamcraft_ffi.ConversationItem
import uniffi.teamcraft_ffi.ConversationKind

class ConversationRightsTest {
  @Test
  fun rightsComeFromTheCoreAndEverythingIsOfferedUntilItIsKnown() {
    assertEquals(ConversationRights(true, true, true, true), ConversationRights.of(null))
    val announcements = item(canPost = false, canReact = true, canManageBookmarks = false, canManageChannel = false)
    assertEquals(ConversationRights(false, true, false, false), ConversationRights.of(announcements))
  }

  @Test
  fun everythingForSomeoneWhoMayDoEverything() {
    assertEquals(
      listOf(
        MessageAction.Reply,
        MessageAction.ReplyInThread,
        MessageAction.Edit,
        MessageAction.Delete,
        MessageAction.Forward,
        MessageAction.Pin,
        MessageAction.Save,
        MessageAction.CopyLink,
        MessageAction.CopyText,
        MessageAction.MarkUnread,
      ),
      actions(ConversationRights(), isMine = true),
    )
  }

  @Test
  fun readersKeepWhatOnlyConcernsThemselves() {
    val reader = ConversationRights(canPost = false, canReact = false, canManageBookmarks = false, canManageChannel = false)
    val expected = listOf(MessageAction.Forward, MessageAction.Save, MessageAction.CopyLink, MessageAction.CopyText, MessageAction.MarkUnread)
    assertEquals("not even their own messages", expected, actions(reader, isMine = true))
    assertEquals(expected, actions(reader, isMine = false))
  }

  @Test
  fun pinsNeedTheirOwnRightAndAcknowledgementsAreForOthersMessages() {
    val member = ConversationRights(canManageBookmarks = false)
    assertEquals(false, MessageAction.Pin in actions(member, isMine = false))
    assertEquals(true, MessageAction.Acknowledge in actions(member, isMine = false, wantsAcknowledgement = true))
    assertEquals(false, MessageAction.Acknowledge in actions(member, isMine = true, wantsAcknowledgement = true))
  }

  @Test
  fun deletedMessagesCannotBeEditedAndThreadsNeedAThreadView() {
    assertEquals(false, MessageAction.Edit in actions(ConversationRights(), isMine = true, isDeleted = true))
    assertEquals(false, MessageAction.ReplyInThread in actions(ConversationRights(), isMine = false, hasThreads = false))
  }

  private fun actions(
    rights: ConversationRights,
    isMine: Boolean,
    isDeleted: Boolean = false,
    hasThreads: Boolean = true,
    wantsAcknowledgement: Boolean = false,
  ) = messageActions(rights, isMine, isDeleted, hasThreads, wantsAcknowledgement)

  private fun item(canPost: Boolean, canReact: Boolean, canManageBookmarks: Boolean, canManageChannel: Boolean) = ConversationItem(
    id = "1",
    maybeSpaceId = "2",
    maybeSpaceName = "Project A",
    kind = ConversationKind.PUBLIC,
    name = "news",
    topic = "",
    unreadCount = 0u,
    mentionCount = 0u,
    isFavorite = false,
    isMuted = false,
    canPost = canPost,
    maybePeerPresence = null,
    canReact = canReact,
    canManageBookmarks = canManageBookmarks,
    canManageChannel = canManageChannel,
  )
}
