package de.zeitgleich.teamcraft.ui

import androidx.compose.ui.graphics.Color
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.teamcraft_ffi.ConversationItem
import uniffi.teamcraft_ffi.ConversationKind
import uniffi.teamcraft_ffi.PeerInfo

class ConversationPeerTest {
  @Test
  fun aDirectMessageWithABotIsMarkedAndAnyOtherIsNot() {
    assertTrue(item(ConversationKind.DIRECT, PeerInfo("7", isBot = true, avatarHue = 7u)).isWithBot())
    assertFalse(item(ConversationKind.DIRECT, PeerInfo("7", isBot = false, avatarHue = 7u)).isWithBot())
    assertFalse("peer not known yet", item(ConversationKind.DIRECT, null).isWithBot())
    assertFalse(item(ConversationKind.GROUP, PeerInfo("7", isBot = true, avatarHue = 7u)).isWithBot())
  }

  @Test
  fun theAvatarTakesTheHueTheCoreNamedElseTheOldDerivation() {
    val named = item(ConversationKind.DIRECT, PeerInfo("7", isBot = false, avatarHue = 200u))
    assertEquals(avatarColorOfHue(200), conversationAvatarColor(named))
    val unknown: Color = conversationAvatarColor(item(ConversationKind.DIRECT, null, peerId = "7"))
    assertEquals(avatarColor("7", "Alex"), unknown)
  }

  private fun item(kind: ConversationKind, peer: PeerInfo?, peerId: String? = peer?.userId) = ConversationItem(
    id = "1",
    maybeSpaceId = null,
    maybeSpaceName = null,
    kind = kind,
    name = "Alex",
    topic = "",
    unreadCount = 0u,
    mentionCount = 0u,
    isFavorite = false,
    isMuted = false,
    canPost = true,
    maybePeerId = peerId,
    maybePeer = peer,
  )
}
