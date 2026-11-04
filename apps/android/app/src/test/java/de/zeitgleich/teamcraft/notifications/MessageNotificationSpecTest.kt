package de.zeitgleich.teamcraft.notifications

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.teamcraft_ffi.NotificationContent
import uniffi.teamcraft_ffi.NotificationGap
import uniffi.teamcraft_ffi.NotificationSuppression
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
    // The lock screen names the sender, never the text.
    assertEquals("Alice", spec.publicTitle)
    assertEquals("from Alice", spec.publicText)
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
    assertEquals("Project A", spec.publicTitle)
    assertEquals("2 from Bob, Alice", spec.publicText)
    assertFalse(spec.publicText.contains("first") || spec.publicText.contains("second"))
  }

  @Test
  fun aPlaceholderStaysOnThePhoneUntilItHasItsText() {
    val placeholder = line("1000", "Alice", "New message").copy(isPlaceholder = true)
    assertTrue(spec(direct(placeholder)).isLocalOnly)
    // Bands and watches get the conversation as soon as its newest message reads.
    assertFalse(spec(direct(placeholder, line("2000", "Alice", "lunch?"))).isLocalOnly)
    assertFalse(spec(direct(line("1000", "Alice", "lunch?"))).isLocalOnly)
  }

  @Test
  fun aFilledInPlaceholderIsPostedForBridgesWithoutAlertingThePhone() {
    val placeholder = direct(line("1000", "Alice", "New message").copy(isPlaceholder = true, maybeArrivedAtMs = NOW))
    val filled = direct(line("1000", "Alice", "lunch?"))
    assertEquals(NotificationAlerting.REFRESH, MessageNotificationSpecs.alerting(false, placeholder, filled, NOW))
    assertEquals(NotificationAlerting.ALERT, MessageNotificationSpecs.alerting(true, placeholder, filled, NOW))
    assertEquals(NotificationAlerting.SILENT, MessageNotificationSpecs.alerting(false, filled, filled, NOW))
    assertEquals(NotificationAlerting.SILENT, MessageNotificationSpecs.alerting(false, null, filled, NOW))
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
    assertEquals("Project A", spec.publicTitle)
    assertEquals("2 from Bob, Alice", spec.publicText)
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
      // Names hidden by the setting: the lock screen names nobody either.
      assertEquals("App", spec.publicTitle)
      assertFalse(spec.publicText.contains("Alice"))
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
    assertEquals("2 conversations", spec.publicTitle)
    assertEquals("Project A, Bob", spec.publicText)
  }

  @Test
  fun withNamesHiddenTheSummaryCounts() {
    val privacy = MessagePrivacy.of(NotificationContent.NOTHING, isAppLockOn = false, isVerified = false)
    val conversations = listOf(channel(line("1000", "Alice", "hi")), direct(line("2000", "Bob", "hey"), line("3000", "Bob", "?")))
    val spec = summary(conversations, isShown = false, privacy = privacy)!!
    assertEquals("3 new", spec.text)
    assertEquals(emptyList<String>(), spec.lines)
    assertEquals("App", spec.publicTitle)
    assertEquals("3 new", spec.publicText)
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
    for (gap in listOf(NotificationGap.WORKSPACE_POLICY, NotificationGap.DEVICE_SETTING, NotificationGap.NOT_SHOWABLE, NotificationGap.HIDDEN, null)) {
      val shown = PushDecisionTest.push(PushNotificationKind.MESSAGE).copy(maybeGap = gap)
      assertFalse(PushRetry.isWanted(shown, attempt = 1))
    }
    val call = PushDecisionTest.push(PushNotificationKind.CALL).copy(maybeGap = NotificationGap.TIMED_OUT)
    assertFalse(PushRetry.isWanted(call, attempt = 1))
  }

  @Test
  fun theRetriesWaitLonger() {
    assertEquals(30_000L, PushRetry.delayMs(1))
    assertEquals(120_000L, PushRetry.delayMs(2))
    assertEquals(600_000L, PushRetry.delayMs(3))
    assertEquals(3_600_000L, PushRetry.delayMs(PushRetry.MAX_ATTEMPTS))
  }

  @Test
  fun aHoldHasABoundAndLongerInACar() {
    assertEquals(20_000L, PushRetry.holdBoundMs(isInCar = false))
    assertEquals(120_000L, PushRetry.holdBoundMs(isInCar = true))
  }

  /** Android Auto and bridges announce what alerts and isn't local-only: one alert, with the text. */
  @Test
  fun aHeldPlaceholderAlertsOnceItHasItsText() {
    val filled = direct(line("1000", "Alice", "lunch?"))
    assertEquals(NotificationAlerting.ALERT, MessageNotificationSpecs.alerting(false, held(NOW), filled, NOW))
    assertEquals(NotificationAlerting.ALERT, MessageNotificationSpecs.alerting(false, held(NOW), filled, NOW, isInCar = true))
  }

  @Test
  fun aPlaceholderThatAlertedStaysSilentOnThePhoneUnlessInACar() {
    val alerted = direct(line("1000", "Alice", "New message").copy(isPlaceholder = true, maybeArrivedAtMs = NOW))
    val filled = direct(line("1000", "Alice", "lunch?"))
    assertEquals(NotificationAlerting.REFRESH, MessageNotificationSpecs.alerting(false, alerted, filled, NOW))
    assertEquals(NotificationAlerting.ALERT, MessageNotificationSpecs.alerting(false, alerted, filled, NOW, isInCar = true))
  }

  /** A text that comes long after the message doesn't alert, in a car neither; bridges still get it. */
  @Test
  fun aLateTextNeverAlerts() {
    val filled = direct(line("1000", "Alice", "lunch?"))
    val late = NOW + 5 * 60_000L + 1
    assertEquals(NotificationAlerting.REFRESH, MessageNotificationSpecs.alerting(false, held(NOW), filled, late))
    assertEquals(NotificationAlerting.REFRESH, MessageNotificationSpecs.alerting(false, held(NOW), filled, late, isInCar = true))
    // A restart: the clock it counts on began anew.
    assertEquals(NotificationAlerting.REFRESH, MessageNotificationSpecs.alerting(false, held(NOW), filled, NOW - 1))
  }

  @Test
  fun aSilentMessageOrAMuteKeepsAFilledInTextQuiet() {
    val filled = direct(line("1000", "Alice", "lunch?"))
    assertEquals(NotificationAlerting.REFRESH, MessageNotificationSpecs.alerting(false, held(NOW), filled, NOW, isSenderSilent = true))
    assertEquals(NotificationAlerting.REFRESH, MessageNotificationSpecs.alerting(false, held(NOW), filled, NOW, isInCar = true, isSenderSilent = true))
    assertEquals(NotificationAlerting.SILENT, MessageNotificationSpecs.alerting(false, held(NOW), filled, NOW, isSuppressed = true))
  }

  @Test
  fun aHoldEndsByWhatIsKnownThen() {
    val push = PushDecisionTest.push(PushNotificationKind.MESSAGE)
    assertEquals(HoldEnd.ALERT, PushRetry.holdEnd(push, NOW, NOW, isAppVisible = false))
    assertEquals(HoldEnd.ALERT, PushRetry.holdEnd(null, NOW, NOW, isAppVisible = false))
    assertEquals(HoldEnd.DROP, PushRetry.holdEnd(push.copy(isRead = true), NOW, NOW, isAppVisible = false))
    assertEquals(
      HoldEnd.SILENT,
      PushRetry.holdEnd(push.copy(maybeSuppression = NotificationSuppression.DO_NOT_DISTURB), NOW, NOW, isAppVisible = false),
    )
    assertEquals(HoldEnd.SILENT, PushRetry.holdEnd(push, NOW, NOW + 5 * 60_000L + 1, isAppVisible = false))
    assertEquals(HoldEnd.SILENT, PushRetry.holdEnd(push, null, NOW, isAppVisible = false))
    assertEquals(HoldEnd.SILENT, PushRetry.holdEnd(push, NOW, NOW, isAppVisible = true))
  }

  /** One re-post at most, and never a second alert for a post that got through. */
  @Test
  fun aLostPostIsPostedAgainAsWhatGotLost() {
    val alerted = PostConfirm().after(10, NotificationAlerting.ALERT)
    assertEquals(null, alerted.repostAs(10))
    assertEquals(NotificationAlerting.ALERT, alerted.repostAs(9))

    val thenSilent = alerted.after(11, NotificationAlerting.SILENT)
    assertEquals(null, thenSilent.repostAs(11))
    assertEquals(NotificationAlerting.ALERT, thenSilent.repostAs(9))
    // The alert got through and only the silent update didn't: no second alert.
    assertEquals(NotificationAlerting.SILENT, thenSilent.repostAs(10))

    val refreshed = PostConfirm().after(20, NotificationAlerting.REFRESH).after(21, NotificationAlerting.SILENT)
    assertEquals(NotificationAlerting.REFRESH, refreshed.repostAs(19))
    assertEquals(NotificationAlerting.SILENT, refreshed.repostAs(20))
    assertEquals(null, refreshed.repostAs(22))
  }

  @Test
  fun aBurstShowsOneQuietNote() {
    assertTrue(PushRetry.isQuietNoteDue(null, NOW))
    assertFalse(PushRetry.isQuietNoteDue(NOW, NOW + 59_999))
    assertTrue(PushRetry.isQuietNoteDue(NOW, NOW + 60_000))
  }

  @Test
  fun aRepeatDoesNotBringBackALineThatWent() {
    val line = line("1000", "Alice", "lunch?")
    assertTrue(NotificationLines.isGone(emptyList(), line, isDuplicate = true))
    assertFalse(NotificationLines.isGone(listOf(line), line, isDuplicate = true))
    assertFalse(NotificationLines.isGone(emptyList(), line, isDuplicate = false))
  }

  /** Every gap has its counted path; only a placeholder may get a text later. */
  @Test
  fun everyGapIsCountedOnItsOwnPath() {
    val push = PushDecisionTest.push(PushNotificationKind.MESSAGE)
    val paths = NotificationGap.entries.associateWith { PushPath.of(push.copy(maybeGap = it)) }
    assertEquals(PushPath.PLACEHOLDER_NO_SESSION, paths[NotificationGap.NO_SESSION])
    assertEquals(PushPath.PLACEHOLDER_NOT_DECRYPTED, paths[NotificationGap.NOT_DECRYPTED])
    assertEquals(PushPath.PLACEHOLDER_TIMED_OUT, paths[NotificationGap.TIMED_OUT])
    assertEquals(PushPath.NOT_SHOWABLE, paths[NotificationGap.NOT_SHOWABLE])
    assertEquals(PushPath.HIDDEN, paths[NotificationGap.HIDDEN])
    assertEquals(PushPath.LIMITED, paths[NotificationGap.WORKSPACE_POLICY])
    assertEquals(PushPath.LIMITED, paths[NotificationGap.DEVICE_SETTING])
    assertEquals(PushPath.SHOWN, PushPath.of(push))
  }

  @Test
  fun aLineThatWasNoMessageGoesAndATextStays() {
    val placeholder = line("1000", "Alice", "New message").copy(isPlaceholder = true)
    val text = line("2000", "Alice", "lunch?")
    assertEquals(listOf(text), NotificationLines.withoutPlaceholder(listOf(placeholder, text), "1000"))
    assertEquals(listOf(placeholder, text), NotificationLines.withoutPlaceholder(listOf(placeholder, text), "2000"))
    assertTrue(NotificationLines.withoutPlaceholder(listOf(placeholder), "1000").isEmpty())
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

  /**
   * Issue #756: a quick burst for one conversation. Every wake-up updates the conversation's one
   * notification with its line, whatever the order they arrive in; a repeated wake-up changes
   * nothing and doesn't alert again.
   */
  @Test
  fun aBurstOfWakeUpsShowsEveryMessageInSendOrder() {
    val arrivals = listOf(
      line("1000", "Alice", "ping") to false,
      line("3000", "Alice", "abc") to false,
      line("2000", "Alice", "pong") to false,
      line("3000", "Alice", "abc") to true,
      line("4000", "Alice", "123") to false,
    )
    var lines = emptyList<NotificationLine>()
    val specs = arrivals.map { (line, isDuplicate) ->
      lines = NotificationLines.withWakeUp(lines, line, isDuplicate)
      spec(direct(*lines.toTypedArray()), isAlerting = !isDuplicate)
    }
    assertEquals(listOf(1, 2, 3, 3, 4), specs.map { it.messageCount })
    val last = specs.last()
    assertEquals(listOf("ping", "pong", "abc", "123"), last.maybeConversation!!.messages.map { it.text })
    assertEquals("123", last.text)
    assertEquals(4000L, last.whenMs)
    assertEquals("4 from Alice", last.publicText)
    val alert = NotificationAlerting.ALERT
    assertEquals(listOf(alert, alert, alert, NotificationAlerting.SILENT, alert), specs.map { it.alerting })
  }

  private fun assertNoText(spec: MessageNotificationSpec) {
    for (shown in listOf(spec.title, spec.text, spec.publicTitle, spec.publicText)) assertFalse(shown.contains("secret"))
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

    const val NOW = 1_000_000L

    fun held(arrivedAtMs: Long) =
      direct(line("1000", "Alice", "New message").copy(isPlaceholder = true, isAlertHeld = true, maybeArrivedAtMs = arrivedAtMs))

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
