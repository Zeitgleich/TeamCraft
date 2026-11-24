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
    // A picture without a file name is still a photo, never "New message".
    assertEquals("IMAGE ", describe(body = null, fileName = "", kind = NotificationAttachmentKind.IMAGE).text)
    assertEquals("VIDEO ", describe(body = null, fileName = null, kind = NotificationAttachmentKind.VIDEO).text)
  }

  @Test
  fun unreadableMessagesFallBackButAreNeverDropped() {
    assertEquals("New message", describe(body = null).text)
    // Without a sender name the conversation names the notification, else the app name.
    assertEquals("planning", describe(conversation = "planning", sender = null).senderName)
    assertEquals("App", describe(conversation = null, sender = null).senderName)
  }

  /** Being added has no text of its own: it says so instead of "New message" (issue #1249). */
  @Test
  fun beingAddedSaysSo() {
    val text = describe(body = null, isAddedYou = true)
    assertEquals("Added you", text.text)
    assertEquals("Alice", text.senderName)
  }

  /** A mention in the channel's notes says so; the page's text never shows (issue #2361). */
  @Test
  fun aMentionInTheNotesSaysSo() {
    val text = describe(body = null, isMentionedInNotes = true)
    assertEquals("Mentioned you in the notes", text.text)
    assertEquals("planning", text.maybeConversationTitle)
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
    isAddedYou: Boolean = false,
    isMentionedInNotes: Boolean = false,
  ) = NotificationText.from(
    maybeConversationName = conversation,
    maybeSenderName = sender,
    maybeText = body,
    maybeFileName = fileName,
    maybeAttachmentKind = kind,
    isDirect = isDirect,
    labels = LABELS,
    isAddedYou = isAddedYou,
    isMentionedInNotes = isMentionedInNotes,
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
      addedYou = "Added you",
      mentionedInNotes = "Mentioned you in the notes",
      newMessageFrom = { "from $it" },
      newMessagesFrom = { count, names -> "$count from $names" },
      newMessages = { count -> "$count new" },
      attachment = { kind, name -> "$kind $name" },
    )
  }
}
