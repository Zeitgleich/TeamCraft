package de.zeitgleich.teamcraft.notifications

import android.Manifest
import android.app.Notification
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.graphics.Typeface
import android.net.Uri
import android.os.Build
import android.text.SpannableString
import android.text.Spanned
import android.text.style.StyleSpan
import android.util.Log
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.app.Person
import androidx.core.app.RemoteInput
import androidx.core.content.ContextCompat
import androidx.core.content.LocusIdCompat
import androidx.core.content.pm.ShortcutInfoCompat
import androidx.core.content.pm.ShortcutManagerCompat
import androidx.core.graphics.drawable.IconCompat
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.security.AppLockPrefs
import de.zeitgleich.teamcraft.ui.MainActivity
import de.zeitgleich.teamcraft.ui.ShareShortcuts
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch
import uniffi.teamcraft_ffi.NotificationAttachmentKind
import uniffi.teamcraft_ffi.PushNotification

/**
 * Message notifications: one MessagingStyle notification per conversation (tag = conversation ID),
 * shown by Android as a conversation through a long-lived shortcut, grouped under a summary once two
 * or more conversations show, with "reply" and "mark as read" actions that work without opening the
 * app (Android Auto and Wear OS need them). A message is posted once, after its wake-up was opened;
 * every later change replaces the notification under the same ID without alerting again, so a watch
 * sees one notification per message (issue #739). What it shows is [MessageNotificationSpecs]; the
 * lines survive a process restart ([NotificationStore]).
 */
class MessageNotifier(
  private val context: Context,
  private val contentPrefs: NotificationContentPrefs,
  private val avatars: AvatarCache,
  private val store: NotificationStore = NotificationStore(context),
) {
  private val manager = NotificationManagerCompat.from(context)
  private val lock = Any()
  private var maybeShown: ShownNotifications? = null

  /** The summary last posted, so an unchanged one is not posted (and mirrored) again. */
  private var maybeLastSummary: SummarySpec? = null
  private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)

  init {
    NotificationChannels.create(context)
  }

  /** Adds a pushed message to its conversation's notification (see [PushDecision.ShowMessage]). */
  fun show(notification: PushNotification, isAlerting: Boolean) {
    val line = line(notification)
    val isMention = notification.isMention || notification.isUrgent
    synchronized(lock) {
      val shown = shown()
      val previous = shown.conversations[notification.conversationId]
      val entry = ConversationNotification(
        conversationId = notification.conversationId,
        maybeTitle = title(notification) ?: previous?.maybeTitle,
        isVerified = notification.isConversationVerified,
        isMention = isMention,
        isDirect = notification.isDirect,
        lines = NotificationLines.withWakeUp(previous?.lines.orEmpty(), line, notification.isDuplicate),
      )
      val badge = notification.maybeBadge?.toInt() ?: shown.maybeBadge
      update(shown.copy(conversations = shown.conversations + (entry.conversationId to entry), maybeBadge = badge))
      post(entry, isAlerting = isAlerting)
      postSummary()
    }
  }

  /**
   * A retry read a message that was shown as a placeholder ([PushRetry]): fills in its line under
   * the same ID, silently. Does nothing when the notification is gone or already complete.
   */
  fun fillIn(notification: PushNotification) {
    val line = line(notification)
    synchronized(lock) {
      val shown = shown()
      val previous = shown.conversations[notification.conversationId] ?: return
      val lines = NotificationLines.filledIn(previous.lines, line) ?: return
      val entry = previous.copy(
        maybeTitle = title(notification) ?: previous.maybeTitle,
        isVerified = previous.isVerified || notification.isConversationVerified,
        lines = lines,
      )
      update(shown.copy(conversations = shown.conversations + (entry.conversationId to entry)))
      post(entry, isAlerting = false)
      postSummary()
    }
  }

  /**
   * A wake-up that could not be opened at all (no push key, a broken store): FCM lowers the
   * priority of apps whose high-priority messages show nothing, and a message must never go
   * unnoticed, so this says "New message" without naming anything.
   */
  fun showUnreadable() {
    if (!canPost()) return
    val notification = NotificationCompat.Builder(context, NotificationChannels.MESSAGES)
      .setSmallIcon(R.drawable.ic_notification)
      .setContentTitle(context.getString(R.string.app_name))
      .setContentText(context.getString(R.string.notification_new_message))
      .setCategory(NotificationCompat.CATEGORY_MESSAGE)
      .setAutoCancel(true)
      .setVisibility(NotificationCompat.VISIBILITY_PRIVATE)
      .setContentIntent(launchIntent())
      .build()
    postSafely(UNREADABLE_TAG, OTHER_ID, notification)
  }

  /** Adds the user's own reply to the conversation's notification (Android Auto expects this). */
  fun showOwnReply(conversationId: String, reply: CharSequence) {
    synchronized(lock) {
      val shown = shown()
      val entry = shown.conversations[conversationId] ?: return
      val line = NotificationLine(text = reply.toString(), sentAtMs = System.currentTimeMillis())
      val updated = entry.copy(lines = NotificationLines.merge(entry.lines, line))
      update(shown.copy(conversations = shown.conversations + (conversationId to updated)))
      post(updated, isAlerting = false)
    }
  }

  /**
   * Read on another device (a `dismiss` wake-up): removes the thread's or the conversation's lines
   * up to the read message; the notification goes when nothing is left.
   */
  fun removeRead(conversationId: String, maybeThreadId: String?, maybeUpToMessageId: String?, maybeBadge: UInt?) {
    synchronized(lock) {
      val shown = shown()
      val withBadge = maybeBadge?.let { shown.copy(maybeBadge = it.toInt()) } ?: shown
      val entry = withBadge.conversations[conversationId]
      if (entry == null) {
        update(withBadge)
        postSummary()
        return
      }
      val lines = NotificationLines.afterRead(entry.lines, maybeThreadId, maybeUpToMessageId)
      if (lines.isEmpty()) {
        update(withBadge.copy(conversations = withBadge.conversations - conversationId))
        manager.cancel(conversationId, MESSAGE_ID)
      } else {
        val updated = entry.copy(lines = lines)
        update(withBadge.copy(conversations = withBadge.conversations + (conversationId to updated)))
        if (lines != entry.lines) post(updated, isAlerting = false)
      }
      postSummary()
    }
  }

  /** The conversation was read here (or its notification swiped away): forget it entirely. */
  fun dismiss(conversationId: String) {
    synchronized(lock) {
      val shown = shown()
      if (conversationId !in shown.conversations) return
      update(shown.copy(conversations = shown.conversations - conversationId))
      manager.cancel(conversationId, MESSAGE_ID)
      postSummary()
    }
  }

  /**
   * Shows the current notifications again, silently, as the app lock now says (turning it on hides
   * what is already in the shade; turning it off brings the text back).
   */
  fun repostAll() {
    synchronized(lock) {
      val shown = shown()
      shown.conversations.values.forEach { entry -> post(entry, isAlerting = false) }
      postSummary()
    }
  }

  /** Conversations that have a notification now. */
  fun shownConversationIds(): Set<String> = synchronized(lock) { shown().conversations.keys }

  /** A ring the device's own mute/DND check kept quiet: a silent note that somebody called. */
  fun showQuietCall(name: String) {
    if (!canPost()) return
    val notification = NotificationCompat.Builder(context, NotificationChannels.OTHER)
      .setSmallIcon(R.drawable.ic_notification)
      .setContentTitle(context.getString(R.string.call_missed))
      .setContentText(name)
      .setCategory(NotificationCompat.CATEGORY_MISSED_CALL)
      .setSilent(true)
      .setAutoCancel(true)
      .setVisibility(NotificationCompat.VISIBILITY_PRIVATE)
      .setContentIntent(launchIntent())
      .build()
    postSafely(QUIET_CALL_TAG, OTHER_ID, notification)
  }

  /** The diagnostics' test push arrived. */
  fun showTest(maybeSentAtMs: Long?) {
    if (!canPost()) return
    val maybeDelaySeconds = maybeSentAtMs?.let { ((System.currentTimeMillis() - it).coerceAtLeast(0) + 500) / 1000 }
    val text = maybeDelaySeconds?.let { context.getString(R.string.test_push_received_after, it) }
      ?: context.getString(R.string.test_push_received)
    val notification = NotificationCompat.Builder(context, NotificationChannels.OTHER)
      .setSmallIcon(R.drawable.ic_notification)
      .setContentTitle(context.getString(R.string.test_push_title))
      .setContentText(text)
      .setCategory(NotificationCompat.CATEGORY_STATUS)
      .setAutoCancel(true)
      .setTimeoutAfter(TEST_TIMEOUT_MS)
      .build()
    postSafely(TEST_TAG, OTHER_ID, notification)
  }

  /** Removes every notification, the shortcuts and what they remembered (sign-out). */
  fun clearAll() {
    synchronized(lock) {
      maybeShown = ShownNotifications()
      maybeLastSummary = null
      store.delete()
      manager.cancelAll()
      avatars.clear()
      runCatching { ShortcutManagerCompat.removeAllDynamicShortcuts(context) }
    }
  }

  private fun line(notification: PushNotification): NotificationLine {
    val text = text(notification)
    val sentAtMs = notification.maybeMessageId?.let(NotificationLines::sentAtMs)
      ?: notification.maybeSentAtMs
      ?: System.currentTimeMillis()
    return NotificationLine(
      maybeMessageId = notification.maybeMessageId,
      maybeThreadId = notification.maybeThreadId,
      maybeSenderKey = notification.maybeSenderId ?: text.senderName,
      senderName = text.senderName,
      text = text.text,
      sentAtMs = sentAtMs,
      isPlaceholder = PushRetry.isPlaceholder(notification),
    )
  }

  private fun title(notification: PushNotification): String? = text(notification).maybeConversationTitle

  private fun text(notification: PushNotification): NotificationText =
    NotificationText.from(
      maybeConversationName = notification.maybeConversationName,
      maybeSenderName = notification.maybeSenderName,
      maybeText = notification.maybeText,
      maybeFileName = notification.maybeFileName,
      maybeAttachmentKind = notification.maybeAttachmentKind,
      isDirect = notification.isDirect,
      labels = labels(),
    )

  private fun shown(): ShownNotifications =
    maybeShown ?: reconciled(store.read()).also { maybeShown = it }

  /**
   * Drops what the system no longer shows (swiped away while no process ran, a reboot), so it
   * doesn't come back with the next message.
   */
  private fun reconciled(saved: ShownNotifications): ShownNotifications {
    val system = context.getSystemService(NotificationManager::class.java) ?: return saved
    val active = runCatching { system.activeNotifications.filter { it.id == MESSAGE_ID }.mapNotNull { it.tag }.toSet() }
      .getOrNull() ?: return saved
    val kept = saved.conversations.filterKeys { it in active }
    return if (kept.size == saved.conversations.size) saved else saved.copy(conversations = kept).also(store::write)
  }

  private fun update(shown: ShownNotifications) {
    maybeShown = shown
    store.write(shown)
  }

  private fun post(entry: ConversationNotification, isAlerting: Boolean) {
    if (!canPost()) return
    val privacy = MessagePrivacy.of(contentPrefs.content.value, isAppLockOn(), entry.isVerified)
    val spec = MessageNotificationSpecs.conversation(entry, privacy, isAlerting, labels()) ?: return
    postSafely(entry.conversationId, MESSAGE_ID, build(spec))
    spec.maybeShortcut?.let(::pushShortcut)
    if (privacy.showsNames) fetchAvatars(entry)
  }

  private fun build(spec: MessageNotificationSpec): Notification {
    val isAlerting = spec.alerting == NotificationAlerting.ALERT
    val builder = NotificationCompat.Builder(context, spec.channel)
      .setSmallIcon(R.drawable.ic_notification)
      .setCategory(NotificationCompat.CATEGORY_MESSAGE)
      // Also with MessagingStyle: watch bridges that read only title and text show these.
      .setContentTitle(spec.title)
      .setContentText(spec.text)
      .setWhen(spec.whenMs)
      .setShowWhen(true)
      .setAutoCancel(true)
      .setOnlyAlertOnce(!isAlerting)
      .setSilent(!isAlerting)
      // Bridged to watches (Wear OS, vendor apps): the default, stated so it stays.
      .setLocalOnly(false)
      .setGroup(GROUP_MESSAGES)
      .setGroupAlertBehavior(NotificationCompat.GROUP_ALERT_CHILDREN)
      .setContentIntent(openIntent(spec.conversationId))
      .setDeleteIntent(actionIntent(NotificationActionReceiver.ACTION_DISMISSED, spec.conversationId, isMutable = false))
      // The lock screen names the sender but not the text unless the user unlocks.
      .setVisibility(NotificationCompat.VISIBILITY_PRIVATE)
      .setPublicVersion(publicVersion(spec.channel, spec.publicTitle, spec.publicText))
    if (spec.messageCount > 1) builder.setNumber(spec.messageCount)
    spec.maybeConversation?.let { conversation -> builder.setStyle(messagingStyle(conversation)) }
    spec.maybeLargeIconSender?.let { sender ->
      builder.setLargeIcon(avatars.maybeBitmap(sender.key) ?: PersonIcons.bitmap(sender.key, sender.name))
    }
    spec.maybeShortcut?.let { shortcut -> builder.setShortcutId(shortcut.id).setLocusId(LocusIdCompat(shortcut.id)) }
    spec.actions.forEach { action ->
      builder.addAction(
        when (action) {
          MessageAction.REPLY -> replyAction(spec.conversationId, spec.maybeReplyThreadId)
          MessageAction.MARK_READ -> markReadAction(spec.conversationId)
          MessageAction.MUTE -> muteAction(spec.conversationId)
        },
      )
    }
    return builder.build()
  }

  private fun messagingStyle(conversation: ConversationStyleSpec): NotificationCompat.MessagingStyle {
    val me = Person.Builder().setName(context.getString(R.string.you)).setKey(SELF_KEY).build()
    val style = NotificationCompat.MessagingStyle(me)
      .setConversationTitle(conversation.maybeConversationTitle)
      .setGroupConversation(conversation.isGroupConversation)
    conversation.messages.forEach { message ->
      style.addMessage(highlighted(message.text), message.sentAtMs, message.maybeSender?.let(::person))
    }
    return style
  }

  /**
   * Fetches the senders' pictures in the background (never from the push); a picture that arrives
   * replaces the initials in the notification already shown, silently.
   */
  private fun fetchAvatars(entry: ConversationNotification) {
    val senders = entry.lines.mapNotNull { it.maybeSenderKey }.distinct()
    senders.forEach { sender ->
      scope.launch {
        if (!avatars.refresh(sender)) return@launch
        synchronized(lock) {
          shown().conversations[entry.conversationId]?.let { current -> post(current, isAlerting = false) }
        }
      }
    }
  }

  private fun person(sender: NotificationSender): Person =
    Person.Builder()
      .setName(sender.name)
      .setKey(sender.key)
      .setIcon(avatars.maybeIcon(sender.key) ?: PersonIcons.of(sender.key, sender.name))
      .build()

  /** `@mentions` in bold, as in the app. */
  private fun highlighted(text: String): CharSequence {
    val ranges = NotificationStyling.mentionRanges(text)
    if (ranges.isEmpty()) return text
    return SpannableString(text).apply {
      ranges.forEach { setSpan(StyleSpan(Typeface.BOLD), it.first, it.last + 1, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE) }
    }
  }

  private fun labels() = NotificationText.Labels(
    appName = context.getString(R.string.app_name),
    newMessage = context.getString(R.string.notification_new_message),
    newMessageFrom = { context.getString(R.string.notification_new_message_from, it) },
    newMessagesFrom = { count, names ->
      context.resources.getQuantityString(R.plurals.notification_new_messages_from, count, count, names)
    },
    newMessages = { count -> context.resources.getQuantityString(R.plurals.notification_new_messages, count, count) },
    attachment = { kind, fileName ->
      when (kind) {
        NotificationAttachmentKind.IMAGE -> context.getString(R.string.notification_photo)
        NotificationAttachmentKind.VIDEO -> context.getString(R.string.notification_video)
        NotificationAttachmentKind.AUDIO -> context.getString(R.string.notification_audio)
        NotificationAttachmentKind.FILE -> context.getString(R.string.notification_file, fileName)
      }
    },
  )

  /**
   * The summary that groups the conversations once two or more show
   * ([MessageNotificationSpecs.summary]); never alerts, carries the badge count, and is posted
   * again only when it changed, so watch bridges that mirror summaries don't repeat it.
   */
  private fun postSummary() {
    val shown = shown()
    if (shown.conversations.isEmpty()) {
      maybeLastSummary = null
      manager.cancel(null, SUMMARY_ID)
      return
    }
    if (!canPost()) return
    // Names only (none when names are hidden): the summary may show where the text does not.
    val privacy = MessagePrivacy.of(contentPrefs.content.value, isAppLockOn(), isVerified = false)
    val isShown = isSummaryShown()
    val spec = MessageNotificationSpecs.summary(
      conversations = shown.conversations.values,
      privacy = privacy,
      maybeBadge = shown.maybeBadge,
      isShown = isShown,
      labels = labels(),
      conversationsTitle = { count -> context.resources.getQuantityString(R.plurals.notification_summary, count, count) },
    ) ?: return
    if (isShown && spec == maybeLastSummary) return
    val inbox = NotificationCompat.InboxStyle().setBigContentTitle(spec.title)
    spec.lines.forEach(inbox::addLine)
    val summary = NotificationCompat.Builder(context, NotificationChannels.MESSAGES)
      .setSmallIcon(R.drawable.ic_notification)
      .setContentTitle(spec.title)
      .setContentText(spec.text)
      .setStyle(inbox)
      .setCategory(NotificationCompat.CATEGORY_MESSAGE)
      .setGroup(GROUP_MESSAGES)
      .setGroupSummary(true)
      .setGroupAlertBehavior(NotificationCompat.GROUP_ALERT_CHILDREN)
      .setSilent(true)
      .setOnlyAlertOnce(true)
      .setLocalOnly(false)
      .setAutoCancel(true)
      .setNumber(spec.number)
      .setBadgeIconType(NotificationCompat.BADGE_ICON_SMALL)
      .setVisibility(NotificationCompat.VISIBILITY_PRIVATE)
      .setPublicVersion(publicVersion(NotificationChannels.MESSAGES, spec.publicTitle, spec.publicText))
      .setContentIntent(launchIntent())
      .build()
    postSafely(null, SUMMARY_ID, summary)
    maybeLastSummary = spec
  }

  /** Whether the system shows the summary now (the process may have restarted since it was posted). */
  private fun isSummaryShown(): Boolean {
    val system = context.getSystemService(NotificationManager::class.java) ?: return maybeLastSummary != null
    return runCatching { system.activeNotifications.any { it.id == SUMMARY_ID && it.tag == null } }
      .getOrElse { maybeLastSummary != null }
  }

  /** A long-lived shortcut per conversation: what makes Android treat the notification as one. */
  private fun pushShortcut(spec: ShortcutSpec) {
    val label = spec.label.ifBlank { context.getString(R.string.app_name) }
    val shortcut = ShortcutInfoCompat.Builder(context, spec.id)
      .setShortLabel(label)
      .setLongLived(true)
      .setLocusId(LocusIdCompat(spec.id))
      // Also a direct share target, so this push doesn't drop the share sheet's entry (#829).
      .setCategories(setOf(ShareShortcuts.CATEGORY))
      .setIcon(shortcutIcon(spec, label))
      .setIntent(openConversationIntent(spec.id).setAction(Intent.ACTION_VIEW))
      .apply { spec.maybePerson?.let { setPerson(person(it)) } }
      .build()
    try {
      ShortcutManagerCompat.pushDynamicShortcut(context, shortcut)
    } catch (failure: Exception) {
      // Rate-limited or refused by the launcher: the notification still shows, just not as a conversation.
      Log.w(TAG, "conversation shortcut not pushed: ${failure.javaClass.simpleName}")
    }
  }

  /** A person's picture for a one-to-one chat, else the generated avatar of the conversation. */
  private fun shortcutIcon(spec: ShortcutSpec, label: String): IconCompat {
    val maybeSender = spec.maybePerson
    return maybeSender?.let { avatars.maybeIcon(it.key) } ?: PersonIcons.of(maybeSender?.key ?: spec.id, label)
  }

  private fun postSafely(maybeTag: String?, id: Int, notification: Notification) {
    try {
      manager.notify(maybeTag, id, notification)
    } catch (denied: SecurityException) {
      // The permission was revoked between the check and the post.
      Log.w(TAG, "notification not posted: permission missing")
    }
  }

  private fun isAppLockOn(): Boolean = AppLockPrefs.prefs(context).isEnabled

  /**
   * What the lock screen shows while it hides sensitive content: who wrote, never what (no names
   * when the setting hides them). Watch bridges that follow the lock screen's privacy show this
   * too while the phone is locked.
   */
  private fun publicVersion(channel: String, title: String, text: String) = NotificationCompat.Builder(context, channel)
    .setSmallIcon(R.drawable.ic_notification)
    .setContentTitle(title)
    .setContentText(text)
    .setCategory(NotificationCompat.CATEGORY_MESSAGE)
    .build()

  /** Replies where the newest message is: in its thread if it was a thread reply. */
  private fun replyAction(conversationId: String, maybeThreadId: String?): NotificationCompat.Action {
    val input = RemoteInput.Builder(NotificationActionReceiver.KEY_REPLY)
      .setLabel(context.getString(R.string.notification_reply))
      .build()
    // Mutable: the system adds the typed or spoken text to this intent.
    val pending = actionIntent(NotificationActionReceiver.ACTION_REPLY, conversationId, isMutable = true, maybeThreadId)
    return NotificationCompat.Action.Builder(R.drawable.ic_notification, context.getString(R.string.notification_reply), pending)
      .addRemoteInput(input)
      .setSemanticAction(NotificationCompat.Action.SEMANTIC_ACTION_REPLY)
      .setShowsUserInterface(false)
      .setAllowGeneratedReplies(true)
      // Not from the lock screen without unlocking (Android 12+; Android Auto is unaffected).
      .setAuthenticationRequired(true)
      .build()
  }

  private fun markReadAction(conversationId: String): NotificationCompat.Action {
    val pending = actionIntent(NotificationActionReceiver.ACTION_MARK_READ, conversationId, isMutable = false)
    return NotificationCompat.Action.Builder(R.drawable.ic_notification, context.getString(R.string.notification_mark_read), pending)
      .setSemanticAction(NotificationCompat.Action.SEMANTIC_ACTION_MARK_AS_READ)
      .setShowsUserInterface(false)
      .setAuthenticationRequired(true)
      .build()
  }

  private fun muteAction(conversationId: String): NotificationCompat.Action {
    val pending = actionIntent(NotificationActionReceiver.ACTION_MUTE, conversationId, isMutable = false)
    return NotificationCompat.Action.Builder(R.drawable.ic_notification, context.getString(R.string.notification_mute), pending)
      .setSemanticAction(NotificationCompat.Action.SEMANTIC_ACTION_MUTE)
      .setShowsUserInterface(false)
      .setAuthenticationRequired(true)
      .build()
  }

  /** Each conversation's intents differ in their data URI, so their PendingIntents never collide. */
  private fun actionIntent(
    action: String,
    conversationId: String,
    isMutable: Boolean,
    maybeThreadId: String? = null,
  ): PendingIntent {
    val intent = NotificationActionReceiver.intent(context, action, conversationId, maybeThreadId)
      .setData(conversationUri(conversationId))
    val mutability = if (isMutable) PendingIntent.FLAG_MUTABLE else PendingIntent.FLAG_IMMUTABLE
    return PendingIntent.getBroadcast(context, 0, intent, PendingIntent.FLAG_UPDATE_CURRENT or mutability)
  }

  private fun openIntent(conversationId: String): PendingIntent {
    val intent = openConversationIntent(conversationId)
      .setData(conversationUri(conversationId))
      .addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_CLEAR_TOP)
    return PendingIntent.getActivity(context, 0, intent, PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE)
  }

  private fun openConversationIntent(conversationId: String): Intent =
    Intent(context, MainActivity::class.java)
      .putExtra(MainActivity.EXTRA_CONVERSATION_ID, conversationId)
      .putExtra(MainActivity.EXTRA_LAUNCH_TOKEN, LaunchToken.of(context))

  private fun launchIntent(): PendingIntent =
    PendingIntent.getActivity(
      context,
      0,
      Intent(context, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP),
      PendingIntent.FLAG_IMMUTABLE,
    )

  private fun conversationUri(conversationId: String): Uri = Uri.fromParts(URI_SCHEME, conversationId, null)

  private fun canPost(): Boolean =
    Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU ||
      ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED

  companion object {
    /** Message notifications are told apart by their tag (the conversation ID); the ID is fixed. */
    const val MESSAGE_ID = 1
    private const val SUMMARY_ID = 2
    private const val OTHER_ID = 3
    private const val TEST_TAG = "test"
    private const val QUIET_CALL_TAG = "quiet_call"
    private const val UNREADABLE_TAG = "unreadable"
    private const val GROUP_MESSAGES = "de.zeitgleich.teamcraft.MESSAGES"
    private const val SELF_KEY = "self"
    private const val URI_SCHEME = "teamcraft-conversation"
    private const val TEST_TIMEOUT_MS = 5L * 60 * 1000
    private const val TAG = "TeamCraftNotify"
  }
}
