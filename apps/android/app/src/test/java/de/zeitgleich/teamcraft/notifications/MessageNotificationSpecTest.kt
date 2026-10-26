package de.zeitgleich.teamcraft.notifications

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.teamcraft_ffi.NotificationContent
import uniffi.teamcraft_ffi.NotificationGap
import uniffi.teamcraft_ffi.PushNotificationKind

class MessageNotificationSpecTest {
  @Test
  fun aDirectMessageNamesTheSenderAndShowsTheText() {
    val spec = spec(direct(line("1000", "Alice", "lunch?")))
    assertEquals("Alice", spec.title)
    assertEquals("lunch?", spec.text)
    val style = spec.maybeConversation!!
    assertNull(style.maybeConversationTitle)
    assertFalse(style.isGroupConversation)
    assertEquals(listOf(StyleMessage("lunch?", 1000, NotificationSender("user-Alice", "Alice"))), style.messages)
    assertEquals(NotificationChannels.DIRECT_MESSAGES, spec.channel)
    assertEquals(NotificationAlerting.ALERT, spec.alerting)
    assertEquals(ShortcutSpec(CONVERSATION, "Alice", NotificationSender("user-Alice", "Alice")), spec.maybeShortcut)
    assertEquals(listOf(MessageAction.REPLY, MessageAction.MARK_READ, MessageAction.MUTE), spec.actions)
    assertNull(spec.maybeLargeIconSender)
  }

  @Test
  fun aChannelIsAGroupConversationWithItsTitle() {
    val entry = channel(line("1000", "Alice", "first"), line("2000", "Bob", "second"))
    val spec = spec(entry)
    assertEquals("Project A", spec.title)
    assertEquals("Bob: second", spec.text)
    val style = spec.maybeConversation!!
    assertEquals("Project A", style.maybeConversationTitle)
    assertTrue(style.isGroupConversation)
    assertEquals(listOf("Alice", "Bob"), style.messages.map { it.maybeSender?.name })
    assertEquals(2, spec.messageCount)
    assertEquals(2000L, spec.whenMs)
    assertEquals(NotificationChannels.MESSAGES, spec.channel)
    // A channel's shortcut is the conversation's, not a person's.
    assertNull(spec.maybeShortcut!!.maybePerson)
  }

  @Test
  fun aDirectChatWithSeveralSendersIsAGroupConversation() {
    val spec = spec(direct(line("1000", "Alice", "hi"), line("2000", "Bob", "hey")))
    assertTrue(spec.maybeConversation!!.isGroupConversation)
  }

  @Test
  fun theUsersOwnReplyIsAMessageWithoutSenderAndNeverTheHeadline() {
    val spec = spec(direct(line("1000", "Alice", "lunch?"), NotificationLine(text = "yes", sentAtMs = 2000)))
    assertEquals("lunch?", spec.text)
    assertEquals(1000L, spec.whenMs)
    assertNull(spec.maybeConversation!!.messages.last().maybeSender)
    assertEquals(1, spec.messageCount)
  }

  @Test
  fun onlyOwnRepliesPostNothing() {
    assertNull(build(direct(NotificationLine(text = "yes", sentAtMs = 2000)), content()))
  }

  @Test
  fun anUpdateDoesNotAlertAgain() {
    assertEquals(NotificationAlerting.SILENT, spec(direct(line("1000", "Alice", "hi")), isAlerting = false).alerting)
  }

  @Test
  fun mentionsAndUrgentGoToTheMentionsChannel() {
    val entry = channel(line("1000", "Alice", "@me look")).copy(isMention = true)
    assertEquals(NotificationChannels.MENTIONS, spec(entry).channel)
  }

  @Test
  fun anUnverifiedConversationOffersNoActions() {
    val privacy = MessagePrivacy.of(NotificationContent.CONTENT, isAppLockOn = false, isVerified = false)
    assertEquals(emptyList<MessageAction>(), build(direct(line("1000", "Alice", "hi")), privacy)!!.actions)
  }

  @Test
  fun aReplyGoesToTheNewestMessagesThread() {
    val entry = channel(line("1000", "Alice", "top"), line("2000", "Bob", "in thread", threadId = "900"))
    assertEquals("900", spec(entry).maybeReplyThreadId)
  }

  // App lock, "sender only" and "nothing": still one notification, with as much as allowed.
  @Test
  fun senderOnlyNamesWhoWroteInTitleAndText() {
    val privacy = MessagePrivacy.of(NotificationContent.SENDER_ONLY, isAppLockOn = false, isVerified = true)
    val spec = build(direct(line("1000", "Alice", "secret")), privacy)!!
    assertEquals("Alice", spec.title)
    assertEquals("from Alice", spec.text)
    assertNull(spec.maybeConversation)
    assertEquals(NotificationSender("user-Alice", "Alice"), spec.maybeLargeIconSender)
    assertTrue(spec.actions.isNotEmpty())
    assertNoText(spec)
  }

  @Test
  fun theAppLockShowsTheSenderWithoutTextActionsOrShortcut() {
    val privacy = MessagePrivacy.of(NotificationContent.CONTENT, isAppLockOn = true, isVerified = true)
    val spec = build(channel(line("1000", "Alice", "secret"), line("2000", "Bob", "secret")), privacy)!!
    assertEquals("Project A", spec.title)
    assertEquals("2 from Bob, Alice", spec.text)
    assertNull(spec.maybeConversation)
    assertNull(spec.maybeShortcut)
    assertEquals(emptyList<MessageAction>(), spec.actions)
    // A channel shows no single sender's picture.
    assertNull(spec.maybeLargeIconSender)
    assertNoText(spec)
  }

  @Test
  fun nothingShowsOnlyNewMessage() {
    val privacy = MessagePrivacy.of(NotificationContent.NOTHING, isAppLockOn = false, isVerified = true)
    val one = build(direct(line("1000", "Alice", "secret")), privacy)!!
    assertEquals("App", one.title)
    assertEquals("New message", one.text)
    val two = build(direct(line("1000", "Alice", "secret"), line("2000", "Alice", "secret")), privacy)!!
    assertEquals("2 new", two.text)
    for (spec in listOf(one, two)) {
      assertNull(spec.maybeConversation)
      assertNull(spec.maybeShortcut)
      assertNull(spec.maybeLargeIconSender)
      assertEquals(emptyList<MessageAction>(), spec.actions)
      assertFalse(spec.title.contains("Alice") || spec.text.contains("Alice"))
    }
  }

  @Test
  fun oneConversationNeedsNoSummary() {
    assertNull(summary(listOf(direct(line("1000", "Alice", "hi"))), isShown = false))
  }

  @Test
  fun aShownSummaryStaysForTheLastConversation() {
    // Cancelling a summary would cancel the conversation's notification with it.
    assertNotNull(summary(listOf(direct(line("1000", "Alice", "hi"))), isShown = true))
  }

  @Test
  fun noConversationNoSummary() {
    assertNull(summary(emptyList(), isShown = true))
  }

  @Test
  fun theSummaryNamesTheConversationsNotASecondNewMessage() {
    val spec = summary(listOf(channel(line("1000", "Alice", "hi")), direct(line("2000", "Bob", "hey"))), isShown = false)!!
    assertEquals("2 conversations", spec.title)
    assertEquals("Project A, Bob", spec.text)
    assertEquals(listOf("Project A", "Bob"), spec.lines)
    assertEquals(7, spec.number)
  }

  @Test
  fun withNamesHiddenTheSummaryCounts() {
    val privacy = MessagePrivacy.of(NotificationContent.NOTHING, isAppLockOn = false, isVerified = false)
    val conversations = listOf(channel(line("1000", "Alice", "hi")), direct(line("2000", "Bob", "hey"), line("3000", "Bob", "?")))
    val spec = summary(conversations, isShown = false, privacy = privacy)!!
    assertEquals("3 new", spec.text)
    assertEquals(emptyList<String>(), spec.lines)
  }

  @Test
  fun aTimedOutOrUndecryptedMessageIsAPlaceholderAndRetried() {
    for (gap in listOf(NotificationGap.TIMED_OUT, NotificationGap.NOT_DECRYPTED, NotificationGap.NO_SESSION)) {
      val shown = PushDecisionTest.push(PushNotificationKind.MESSAGE).copy(maybeGap = gap, maybeText = null)
      assertTrue(gap.name, PushRetry.isPlaceholder(shown))
      assertTrue(PushRetry.isWanted(shown, attempt = 1))
      assertTrue(PushRetry.isWanted(shown, attempt = PushRetry.MAX_ATTEMPTS))
      assertFalse(PushRetry.isWanted(shown, attempt = PushRetry.MAX_ATTEMPTS + 1))
    }
  }

  @Test
  fun aPolicyOrSettingCutIsNotRetried() {
    for (gap in listOf(NotificationGap.WORKSPACE_POLICY, NotificationGap.DEVICE_SETTING, NotificationGap.NOT_SHOWABLE, null)) {
      val shown = PushDecisionTest.push(PushNotificationKind.MESSAGE).copy(maybeGap = gap)
      assertFalse(PushRetry.isWanted(shown, attempt = 1))
    }
    val call = PushDecisionTest.push(PushNotificationKind.CALL).copy(maybeGap = NotificationGap.TIMED_OUT)
    assertFalse(PushRetry.isWanted(call, attempt = 1))
  }

  @Test
  fun theRetriesWaitLonger() {
    assertEquals(30_000L, PushRetry.delayMs(1))
    assertEquals(60_000L, PushRetry.delayMs(2))
  }

  @Test
  fun aRetryFillsInThePlaceholderUnderTheSameLine() {
    val placeholder = line("1000", "Alice", "New message").copy(isPlaceholder = true)
    val filled = NotificationLines.filledIn(listOf(placeholder), line("1000", "Alice", "lunch?"))
    assertEquals(listOf("lunch?"), filled?.map { it.text })
  }

  @Test
  fun aRetryNeverBringsBackOrOverwrites() {
    val read = line("1000", "Alice", "lunch?")
    // Gone (read or swiped away), already complete, or still unreadable: nothing to post.
    assertNull(NotificationLines.filledIn(emptyList(), read))
    assertNull(NotificationLines.filledIn(listOf(read), read.copy(text = "edited")))
    val placeholder = read.copy(text = "New message", isPlaceholder = true)
    assertNull(NotificationLines.filledIn(listOf(placeholder), placeholder))
  }

  @Test
  fun aSecondWakeUpReplacesAPlaceholderButKeepsARealLine() {
    val placeholder = line("1000", "Alice", "New message").copy(isPlaceholder = true)
    val read = line("1000", "Alice", "lunch?")
    assertEquals(listOf("lunch?"), NotificationLines.withWakeUp(listOf(placeholder), read, isDuplicate = true).map { it.text })
    assertEquals(listOf("lunch?"), NotificationLines.withWakeUp(listOf(read), placeholder, isDuplicate = true).map { it.text })
    // Not a duplicate (an urgent repeat): replaces.
    assertEquals(listOf("again"), NotificationLines.withWakeUp(listOf(read), read.copy(text = "again"), isDuplicate = false).map { it.text })
  }

  private fun assertNoText(spec: MessageNotificationSpec) {
    assertFalse(spec.title.contains("secret") || spec.text.contains("secret"))
  }

  private fun summary(
    conversations: List<ConversationNotification>,
    isShown: Boolean,
    privacy: MessagePrivacy = content(),
  ) = MessageNotificationSpecs.summary(conversations, privacy, maybeBadge = 7, isShown = isShown, labels = LABELS) { "$it conversations" }

  companion object {
    const val CONVERSATION = "4242"

    val LABELS = NotificationText.Labels(
      appName = "App",
      newMessage = "New message",
      newMessageFrom = { "from $it" },
      newMessagesFrom = { count, names -> "$count from $names" },
      newMessages = { count -> "$count new" },
      attachment = { kind, name -> "$kind $name" },
    )

    fun content() = MessagePrivacy.of(NotificationContent.CONTENT, isAppLockOn = false, isVerified = true)

    fun spec(entry: ConversationNotification, isAlerting: Boolean = true): MessageNotificationSpec =
      build(entry, content(), isAlerting)!!

    fun build(entry: ConversationNotification, privacy: MessagePrivacy, isAlerting: Boolean = true) =
      MessageNotificationSpecs.conversation(entry, privacy, isAlerting, LABELS)

    fun direct(vararg lines: NotificationLine) =
      ConversationNotification(conversationId = CONVERSATION, isVerified = true, isDirect = true, lines = lines.toList())

    fun channel(vararg lines: NotificationLine) =
      ConversationNotification(conversationId = "4343", maybeTitle = "Project A", isVerified = true, lines = lines.toList())

    fun line(id: String, sender: String, text: String, threadId: String? = null) = NotificationLine(
      maybeMessageId = id,
      maybeThreadId = threadId,
      maybeSenderKey = "user-$sender",
      senderName = sender,
      text = text,
      sentAtMs = id.toLong(),
    )
  }
}
