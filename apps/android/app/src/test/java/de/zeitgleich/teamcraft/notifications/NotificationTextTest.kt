package de.zeitgleich.teamcraft.notifications

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class NotificationTextTest {
  @Test
  fun channelMessagesShowConversationSenderAndText() {
    val text = describe(conversation = "planning", sender = "Alice", body = "lunch?", isDirect = false)
    assertEquals("planning", text.maybeConversationTitle)
    assertEquals("Alice", text.senderName)
    assertEquals("lunch?", text.text)
  }

  @Test
  fun directMessagesHaveNoConversationTitle() {
    assertNull(describe(conversation = "Alice", sender = "Alice", body = "hi", isDirect = true).maybeConversationTitle)
  }

  @Test
  fun filesAndUnreadableMessagesUseTheFallbacks() {
    assertEquals("FILE notes.txt", describe(body = null, fileName = "notes.txt").text)
    assertEquals("New message", describe(body = null).text)
    // Without a sender name the conversation names the notification, else the app name.
    assertEquals("planning", describe(conversation = "planning", sender = null).senderName)
    assertEquals("App", describe(conversation = null, sender = null).senderName)
  }

  private fun describe(
    conversation: String? = "planning",
    sender: String? = "Alice",
    body: String? = "hello",
    fileName: String? = null,
    isDirect: Boolean = false,
  ) = NotificationText.from(
    maybeConversationName = conversation,
    maybeSenderName = sender,
    maybeText = body,
    maybeFileName = fileName,
    isDirect = isDirect,
    fallbackSender = "App",
    fallbackText = "New message",
    fileText = { "FILE $it" },
  )
}
