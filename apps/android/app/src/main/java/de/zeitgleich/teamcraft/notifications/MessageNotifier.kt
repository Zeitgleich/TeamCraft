package de.zeitgleich.teamcraft.notifications

import android.Manifest
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
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
import de.zeitgleich.teamcraft.ui.MainActivity
import uniffi.teamcraft_ffi.PushNotification

/**
 * Message notifications: one MessagingStyle notification per conversation (tag = conversation ID),
 * shown by Android as a conversation through a long-lived shortcut, grouped under a summary, with
 * "reply" and "mark as read" actions that work without opening the app (Android Auto needs them).
 * The lines survive a process restart ([NotificationStore]).
 */
class MessageNotifier(
  private val context: Context,
  private val store: NotificationStore = NotificationStore(context),
) {
  private val manager = NotificationManagerCompat.from(context)
  private val lock = Any()
  private var maybeShown: ShownNotifications? = null

  init {
    NotificationChannels.create(context)
  }

  /** Adds a pushed message to its conversation's notification (see [PushDecision.ShowMessage]). */
  fun show(notification: PushNotification, isAlerting: Boolean) {
    val text = NotificationText.from(
      maybeConversationName = notification.maybeConversationName,
      maybeSenderName = notification.maybeSenderName,
      maybeText = notification.maybeText,
      maybeFileName = notification.maybeFileName,
      isDirect = notification.isDirect,
      fallbackSender = context.getString(R.string.app_name),
      fallbackText = context.getString(R.string.notification_new_message),
      fileText = { context.getString(R.string.notification_file, it) },
    )
    val sentAtMs = notification.maybeMessageId?.let(NotificationLines::sentAtMs)
      ?: notification.maybeSentAtMs
      ?: System.currentTimeMillis()
    val line = NotificationLine(
      maybeMessageId = notification.maybeMessageId,
      maybeThreadId = notification.maybeThreadId,
      maybeSenderKey = notification.maybeSenderId ?: text.senderName,
      senderName = text.senderName,
      text = text.text,
      sentAtMs = sentAtMs,
    )
    val isMention = notification.isDirect || notification.isMention || notification.isUrgent
    synchronized(lock) {
      val shown = shown()
      val previous = shown.conversations[notification.conversationId]
      val lines = previous?.lines.orEmpty()
      // A second wake-up for a shown message re-posts it as it was (it may carry less text).
      val isKnown = notification.isDuplicate && lines.any { it.maybeMessageId == line.maybeMessageId }
      val entry = ConversationNotification(
        conversationId = notification.conversationId,
        maybeTitle = text.maybeConversationTitle ?: previous?.maybeTitle,
        isVerified = notification.isConversationVerified,
        isMention = isMention,
        lines = if (isKnown) lines else NotificationLines.merge(lines, line),
      )
      val badge = notification.maybeBadge?.toInt() ?: shown.maybeBadge
      update(shown.copy(conversations = shown.conversations + (entry.conversationId to entry), maybeBadge = badge))
      post(entry, isAlerting = isAlerting)
      postSummary()
    }
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
      store.delete()
      manager.cancelAll()
      runCatching { ShortcutManagerCompat.removeAllDynamicShortcuts(context) }
    }
  }

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
    val newest = entry.lines.lastOrNull { !it.isOwn } ?: return
    val me = Person.Builder().setName(context.getString(R.string.you)).setKey(SELF_KEY).build()
    val style = NotificationCompat.MessagingStyle(me)
      .setConversationTitle(entry.maybeTitle)
      .setGroupConversation(entry.maybeTitle != null)
    entry.lines.forEach { line -> style.addMessage(line.text, line.sentAtMs, person(line)) }
    val channel = if (entry.isMention) NotificationChannels.MENTIONS else NotificationChannels.MESSAGES
    val builder = NotificationCompat.Builder(context, channel)
      .setSmallIcon(R.drawable.ic_notification)
      .setStyle(style)
      .setCategory(NotificationCompat.CATEGORY_MESSAGE)
      .setWhen(newest.sentAtMs)
      .setShowWhen(true)
      .setAutoCancel(true)
      .setOnlyAlertOnce(!isAlerting)
      .setSilent(!isAlerting)
      .setGroup(GROUP_MESSAGES)
      .setGroupAlertBehavior(NotificationCompat.GROUP_ALERT_CHILDREN)
      .setContentIntent(openIntent(entry.conversationId))
      .setDeleteIntent(actionIntent(NotificationActionReceiver.ACTION_DISMISSED, entry.conversationId, isMutable = false))
      .setShortcutId(entry.conversationId)
      .setLocusId(LocusIdCompat(entry.conversationId))
      // The lock screen shows only "New message" unless the user unlocks.
      .setVisibility(NotificationCompat.VISIBILITY_PRIVATE)
      .setPublicVersion(publicVersion(channel))
    if (entry.isVerified) {
      builder.addAction(replyAction(entry.conversationId)).addAction(markReadAction(entry.conversationId))
    }
    pushShortcut(entry, newest)
    postSafely(entry.conversationId, MESSAGE_ID, builder.build())
  }

  private fun person(line: NotificationLine): Person? {
    val key = line.maybeSenderKey ?: return null
    return Person.Builder().setName(line.senderName).setKey(key).build()
  }

  /** The summary that groups the conversations; never alerts, carries the badge count. */
  private fun postSummary() {
    val shown = shown()
    if (shown.conversations.isEmpty()) {
      manager.cancel(null, SUMMARY_ID)
      return
    }
    if (!canPost()) return
    val count = shown.conversations.size
    val title = context.resources.getQuantityString(R.plurals.notification_summary, count, count)
    val inbox = NotificationCompat.InboxStyle().setBigContentTitle(title)
    // Names only: the summary may show where the conversations' text does not.
    shown.conversations.values.forEach { entry ->
      inbox.addLine(entry.maybeTitle ?: entry.lines.lastOrNull { !it.isOwn }?.senderName.orEmpty())
    }
    val summary = NotificationCompat.Builder(context, NotificationChannels.MESSAGES)
      .setSmallIcon(R.drawable.ic_notification)
      .setContentTitle(title)
      .setStyle(inbox)
      .setCategory(NotificationCompat.CATEGORY_MESSAGE)
      .setGroup(GROUP_MESSAGES)
      .setGroupSummary(true)
      .setGroupAlertBehavior(NotificationCompat.GROUP_ALERT_CHILDREN)
      .setSilent(true)
      .setAutoCancel(true)
      .setNumber(shown.maybeBadge ?: count)
      .setBadgeIconType(NotificationCompat.BADGE_ICON_SMALL)
      .setVisibility(NotificationCompat.VISIBILITY_PRIVATE)
      .setContentIntent(launchIntent())
      .build()
    postSafely(null, SUMMARY_ID, summary)
  }

  /** A long-lived shortcut per conversation: what makes Android treat the notification as one. */
  private fun pushShortcut(entry: ConversationNotification, newest: NotificationLine) {
    val label = entry.maybeTitle ?: newest.senderName
    val shortcut = ShortcutInfoCompat.Builder(context, entry.conversationId)
      .setShortLabel(label.ifBlank { context.getString(R.string.app_name) })
      .setLongLived(true)
      .setLocusId(LocusIdCompat(entry.conversationId))
      .setIcon(IconCompat.createWithResource(context, R.mipmap.ic_launcher))
      .setIntent(openConversationIntent(entry.conversationId).setAction(Intent.ACTION_VIEW))
      .apply { if (entry.maybeTitle == null) person(newest)?.let { setPerson(it) } }
      .build()
    try {
      ShortcutManagerCompat.pushDynamicShortcut(context, shortcut)
    } catch (failure: Exception) {
      // Rate-limited or refused by the launcher: the notification still shows, just not as a conversation.
      Log.w(TAG, "conversation shortcut not pushed: ${failure.javaClass.simpleName}")
    }
  }

  private fun postSafely(maybeTag: String?, id: Int, notification: android.app.Notification) {
    try {
      manager.notify(maybeTag, id, notification)
    } catch (denied: SecurityException) {
      // The permission was revoked between the check and the post.
      Log.w(TAG, "notification not posted: permission missing")
    }
  }

  private fun publicVersion(channel: String) = NotificationCompat.Builder(context, channel)
    .setSmallIcon(R.drawable.ic_notification)
    .setContentTitle(context.getString(R.string.app_name))
    .setContentText(context.getString(R.string.notification_new_message))
    .setCategory(NotificationCompat.CATEGORY_MESSAGE)
    .build()

  private fun replyAction(conversationId: String): NotificationCompat.Action {
    val input = RemoteInput.Builder(NotificationActionReceiver.KEY_REPLY)
      .setLabel(context.getString(R.string.notification_reply))
      .build()
    // Mutable: the system adds the typed or spoken text to this intent.
    val pending = actionIntent(NotificationActionReceiver.ACTION_REPLY, conversationId, isMutable = true)
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

  /** Each conversation's intents differ in their data URI, so their PendingIntents never collide. */
  private fun actionIntent(action: String, conversationId: String, isMutable: Boolean): PendingIntent {
    val intent = NotificationActionReceiver.intent(context, action, conversationId).setData(conversationUri(conversationId))
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
    private const val GROUP_MESSAGES = "de.zeitgleich.teamcraft.MESSAGES"
    private const val SELF_KEY = "self"
    private const val URI_SCHEME = "teamcraft-conversation"
    private const val TEST_TIMEOUT_MS = 5L * 60 * 1000
    private const val TAG = "TeamCraftNotify"
  }
}
