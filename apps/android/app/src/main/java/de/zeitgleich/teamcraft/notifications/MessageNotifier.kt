package de.zeitgleich.teamcraft.notifications

import android.Manifest
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.app.Person
import androidx.core.app.RemoteInput
import androidx.core.content.ContextCompat
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.ui.MainActivity
import uniffi.teamcraft_ffi.PushNotification

/**
 * Message notifications in MessagingStyle with "reply" and "mark as read" actions that work without
 * opening the app, which is what Android Auto needs to read messages aloud and send answers.
 */
class MessageNotifier(private val context: Context) {
  private val manager = NotificationManagerCompat.from(context)
  /** Recent lines per conversation, so a notification shows the conversation, not one message. */
  private val history = mutableMapOf<String, ArrayDeque<NotificationCompat.MessagingStyle.Message>>()

  init {
    createChannels()
  }

  fun show(notification: PushNotification) {
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
    val sender = Person.Builder()
      .setName(text.senderName)
      .setKey(notification.maybeSenderId ?: text.senderName)
      .build()
    val lines = history.getOrPut(notification.conversationId) { ArrayDeque() }
    lines.addLast(NotificationCompat.MessagingStyle.Message(text.text, System.currentTimeMillis(), sender))
    while (lines.size > MAX_LINES) lines.removeFirst()
    val channel = if (notification.isDirect || notification.isMention) CHANNEL_MENTIONS else CHANNEL_MESSAGES
    post(notification.conversationId, text.maybeConversationTitle, channel, isSilent = notification.isSilent)
  }

  /** Adds the user's own reply to the conversation's notification (Android Auto expects this). */
  fun showOwnReply(conversationId: String, reply: CharSequence) {
    val lines = history[conversationId] ?: return
    lines.addLast(NotificationCompat.MessagingStyle.Message(reply, System.currentTimeMillis(), null as Person?))
    post(conversationId, null, CHANNEL_MESSAGES, isSilent = true)
  }

  fun dismiss(conversationId: String) {
    history.remove(conversationId)
    manager.cancel(notificationId(conversationId))
  }

  private fun post(conversationId: String, maybeTitle: String?, channel: String, isSilent: Boolean = false) {
    if (!canPost()) return
    val me = Person.Builder().setName(context.getString(R.string.you)).setKey(SELF_KEY).build()
    val style = NotificationCompat.MessagingStyle(me)
      .setConversationTitle(maybeTitle)
      .setGroupConversation(maybeTitle != null)
    history[conversationId]?.forEach { style.addMessage(it) }
    val builder = NotificationCompat.Builder(context, channel)
      .setSmallIcon(R.drawable.ic_notification)
      .setStyle(style)
      .setCategory(NotificationCompat.CATEGORY_MESSAGE)
      .setAutoCancel(true)
      .setOnlyAlertOnce(isSilent)
      .setSilent(isSilent)
      .setContentIntent(openIntent(conversationId))
      .addAction(replyAction(conversationId))
      .addAction(markReadAction(conversationId))
    manager.notify(notificationId(conversationId), builder.build())
  }

  private fun replyAction(conversationId: String): NotificationCompat.Action {
    val input = RemoteInput.Builder(NotificationActionReceiver.KEY_REPLY)
      .setLabel(context.getString(R.string.notification_reply))
      .build()
    val intent = NotificationActionReceiver.intent(context, NotificationActionReceiver.ACTION_REPLY, conversationId)
    // Mutable: the system adds the typed or spoken text to this intent.
    val pending = PendingIntent.getBroadcast(
      context,
      requestCode(conversationId, 1),
      intent,
      PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_MUTABLE,
    )
    return NotificationCompat.Action.Builder(R.drawable.ic_notification, context.getString(R.string.notification_reply), pending)
      .addRemoteInput(input)
      .setSemanticAction(NotificationCompat.Action.SEMANTIC_ACTION_REPLY)
      .setShowsUserInterface(false)
      .setAllowGeneratedReplies(true)
      .build()
  }

  private fun markReadAction(conversationId: String): NotificationCompat.Action {
    val intent = NotificationActionReceiver.intent(context, NotificationActionReceiver.ACTION_MARK_READ, conversationId)
    val pending = PendingIntent.getBroadcast(
      context,
      requestCode(conversationId, 2),
      intent,
      PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
    )
    return NotificationCompat.Action.Builder(R.drawable.ic_notification, context.getString(R.string.notification_mark_read), pending)
      .setSemanticAction(NotificationCompat.Action.SEMANTIC_ACTION_MARK_AS_READ)
      .setShowsUserInterface(false)
      .build()
  }

  private fun openIntent(conversationId: String): PendingIntent {
    val intent = Intent(context, MainActivity::class.java)
      .putExtra(MainActivity.EXTRA_CONVERSATION_ID, conversationId)
      .addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_CLEAR_TOP)
    return PendingIntent.getActivity(
      context,
      requestCode(conversationId, 0),
      intent,
      PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
    )
  }

  private fun canPost(): Boolean =
    Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU ||
      ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED

  private fun createChannels() {
    val system = context.getSystemService(NotificationManager::class.java) ?: return
    system.createNotificationChannel(
      NotificationChannel(CHANNEL_MESSAGES, context.getString(R.string.notification_channel_messages), NotificationManager.IMPORTANCE_DEFAULT),
    )
    system.createNotificationChannel(
      NotificationChannel(CHANNEL_MENTIONS, context.getString(R.string.notification_channel_mentions), NotificationManager.IMPORTANCE_HIGH),
    )
  }

  private fun notificationId(conversationId: String): Int = conversationId.hashCode()

  private fun requestCode(conversationId: String, action: Int): Int = conversationId.hashCode() * 4 + action

  private companion object {
    const val CHANNEL_MESSAGES = "messages"
    const val CHANNEL_MENTIONS = "mentions"
    const val SELF_KEY = "self"
    const val MAX_LINES = 8
  }
}
