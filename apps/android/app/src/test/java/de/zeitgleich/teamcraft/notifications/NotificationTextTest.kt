package de.zeitgleich.teamcraft.notifications

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import uniffi.teamcraft_ffi.NotificationAttachmentKind

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
  fun filesAreLabelledByKind() {
    assertEquals("IMAGE photo.jpg", describe(body = null, fileName = "photo.jpg", kind = NotificationAttachmentKind.IMAGE).text)
    assertEquals("FILE notes.txt", describe(body = null, fileName = "notes.txt", kind = NotificationAttachmentKind.FILE).text)
    // An older core that names no kind: a plain file.
    assertEquals("FILE notes.txt", describe(body = null, fileName = "notes.txt", kind = null).text)
  }

  @Test
  fun unreadableMessagesFallBackButAreNeverDropped() {
    assertEquals("New message", describe(body = null).text)
    // Without a sender name the conversation names the notification, else the app name.
    assertEquals("planning", describe(conversation = "planning", sender = null).senderName)
    assertEquals("App", describe(conversation = null, sender = null).senderName)
  }

  @Test
  fun senderOnlyNamesWhoWroteNotWhat() {
    assertEquals("from Alice", NotificationText.senderOnly(listOf(line("Alice", 1)), LABELS))
    val several = listOf(line("Alice", 1), line("Bob", 2), line("Alice", 3), own(4))
    // Newest sender first; the user's own replies don't count.
    assertEquals("3 from Alice, Bob", NotificationText.senderOnly(several, LABELS))
    assertEquals("New message", NotificationText.senderOnly(listOf(line("", 1)), LABELS))
  }

  private fun describe(
    conversation: String? = "planning",
    sender: String? = "Alice",
    body: String? = "hello",
    fileName: String? = null,
    kind: NotificationAttachmentKind? = null,
    isDirect: Boolean = false,
  ) = NotificationText.from(
    maybeConversationName = conversation,
    maybeSenderName = sender,
    maybeText = body,
    maybeFileName = fileName,
    maybeAttachmentKind = kind,
    isDirect = isDirect,
    labels = LABELS,
  )

  private fun line(sender: String, sentAtMs: Long) = NotificationLine(
    maybeMessageId = sentAtMs.toString(),
    maybeSenderKey = "user-$sender",
    senderName = sender,
    text = "secret",
    sentAtMs = sentAtMs,
  )

  private fun own(sentAtMs: Long) = NotificationLine(text = "mine", sentAtMs = sentAtMs)

  private companion object {
    val LABELS = NotificationText.Labels(
      appName = "App",
      newMessage = "New message",
      newMessageFrom = { "from $it" },
      newMessagesFrom = { count, names -> "$count from $names" },
      attachment = { kind, name -> "$kind $name" },
    )
  }
}
