package de.zeitgleich.teamcraft.notifications

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.content.Context
import android.content.Intent
import android.media.AudioAttributes
import android.media.RingtoneManager
import android.os.Build
import android.provider.Settings
import android.service.notification.NotificationListenerService
import androidx.annotation.StringRes
import de.zeitgleich.teamcraft.R

/**
 * The app's notification channels (notifications.md, "Android channels"). Importance can't change
 * after a channel was created (the user owns it then), so a channel that needs another importance
 * gets a new ID: incoming calls ring on [INCOMING_CALLS], the ongoing call stays on [CALLS], and
 * channel messages moved from [LEGACY_MESSAGES] (default importance) to [MESSAGES] (high).
 *
 * Every message channel is high: the server pushes only what the user's own level lets through
 * (all, mentions, nothing; mute and quiet hours), so a pushed channel message is one the user asked
 * to hear about, as loud as a direct message. Android Auto announces only heads-up notifications,
 * which a default channel never makes. A conversation the user adjusts in the system settings gets
 * its own channel from the system (`setConversationId`, keyed by the conversation shortcut), which
 * the system picks for its notifications by itself.
 */
object NotificationChannels {
  const val DIRECT_MESSAGES = "direct_messages"
  const val MESSAGES = "channel_messages"

  /**
   * The channel messages' channel up to 1.0.41, at default importance. It goes once nothing shows
   * on it, unless the system holds conversations the user adjusted under it: they keep posting
   * there, so the user's choice for them stays ([parentFor]).
   */
  const val LEGACY_MESSAGES = "messages"
  const val MENTIONS = "mentions"
  const val INCOMING_CALLS = "incoming_calls"
  const val CALLS = "calls"
  const val MEETINGS = "meetings"
  /** Admins asking to join a channel the user owns (admin.md §2.4.1). */
  const val JOIN_REQUESTS = "join_requests"
  const val OTHER = "other"

  private val RING_VIBRATION = longArrayOf(0, 1_000, 1_000)

  /** What the diagnostics list, in the order they show them. */
  val all: List<Channel> = listOf(
    Channel(DIRECT_MESSAGES, R.string.notification_channel_direct_messages, R.string.notification_channel_direct_messages_description, NotificationManager.IMPORTANCE_HIGH),
    Channel(MESSAGES, R.string.notification_channel_messages, R.string.notification_channel_messages_description, NotificationManager.IMPORTANCE_HIGH),
    Channel(MENTIONS, R.string.notification_channel_mentions, R.string.notification_channel_mentions_description, NotificationManager.IMPORTANCE_HIGH),
    Channel(INCOMING_CALLS, R.string.notification_channel_incoming_calls, R.string.notification_channel_incoming_calls_description, NotificationManager.IMPORTANCE_HIGH),
    Channel(CALLS, R.string.call_channel, R.string.notification_channel_calls_description, NotificationManager.IMPORTANCE_DEFAULT),
    Channel(MEETINGS, R.string.notification_channel_meetings, R.string.notification_channel_meetings_description, NotificationManager.IMPORTANCE_HIGH),
    Channel(JOIN_REQUESTS, R.string.notification_channel_join_requests, R.string.notification_channel_join_requests_description, NotificationManager.IMPORTANCE_DEFAULT),
    Channel(OTHER, R.string.notification_channel_other, R.string.notification_channel_other_description, NotificationManager.IMPORTANCE_LOW),
  )

  /**
   * Creates the channels, or updates their names and descriptions (the language may have changed;
   * importance and sound stay as the user set them). The first time, [MESSAGES] takes over what the
   * user set for [LEGACY_MESSAGES] in the system settings, which then goes ([retireLegacy]).
   */
  fun create(context: Context) {
    val manager = context.getSystemService(NotificationManager::class.java) ?: return
    val maybeLegacy = manager.getNotificationChannel(LEGACY_MESSAGES)
    val maybeMovingFrom = maybeLegacy?.takeIf { manager.getNotificationChannel(MESSAGES) == null }
    manager.createNotificationChannels(
      all.map { channel ->
        val built = channel.build(context)
        val maybeFrom = maybeMovingFrom?.takeIf { channel.id == MESSAGES }
        if (maybeFrom != null) built.takeOverFrom(maybeFrom) else built
      },
    )
    if (maybeLegacy != null) retireLegacy(context, manager)
  }

  /**
   * The channel a channel message of `conversationId` is posted on: [LEGACY_MESSAGES] if the user
   * adjusted this conversation there (Android 11+ keeps a conversation's own channel under the
   * channel it was adjusted from), else [MESSAGES].
   */
  fun parentFor(context: Context, conversationId: String): String {
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.R) return MESSAGES
    val manager = context.getSystemService(NotificationManager::class.java) ?: return MESSAGES
    val maybeOwn = runCatching { manager.getNotificationChannel(LEGACY_MESSAGES, conversationId) }.getOrNull()
    return if (maybeOwn?.conversationId == conversationId) LEGACY_MESSAGES else MESSAGES
  }

  /**
   * The old channel goes once nothing shows on it (deleting a channel takes its notifications
   * along) and no conversation the user adjusted lives under it; else it stays, named as the
   * earlier settings, and the next start looks again.
   */
  private fun retireLegacy(context: Context, manager: NotificationManager) {
    val isShowing = runCatching { manager.activeNotifications.any { it.notification.channelId == LEGACY_MESSAGES } }.getOrDefault(true)
    val hasConversations = Build.VERSION.SDK_INT >= Build.VERSION_CODES.R &&
      runCatching { manager.notificationChannels.any { it.parentChannelId == LEGACY_MESSAGES } }.getOrDefault(true)
    if (!isShowing && !hasConversations) {
      manager.deleteNotificationChannel(LEGACY_MESSAGES)
      return
    }
    // Only the name and description change; importance and sound stay the user's.
    val legacy = NotificationChannel(
      LEGACY_MESSAGES,
      context.getString(R.string.notification_channel_messages_earlier),
      NotificationManager.IMPORTANCE_DEFAULT,
    ).apply { description = context.getString(R.string.notification_channel_messages_earlier_description) }
    manager.createNotificationChannel(legacy)
  }

  /**
   * What the user changed of the old channel in the system settings: its importance when they set
   * one (turned off, silent, or high), its sound, vibration, badge and lock-screen visibility.
   * Left alone, the old default importance becomes this channel's high.
   */
  private fun NotificationChannel.takeOverFrom(legacy: NotificationChannel): NotificationChannel = apply {
    val hasUserImportance = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
      legacy.hasUserSetImportance()
    } else {
      legacy.importance != NotificationManager.IMPORTANCE_DEFAULT
    }
    if (hasUserImportance) importance = legacy.importance
    if (legacy.sound != Settings.System.DEFAULT_NOTIFICATION_URI) setSound(legacy.sound, legacy.audioAttributes)
    enableVibration(legacy.shouldVibrate())
    legacy.vibrationPattern?.let { vibrationPattern = it }
    setShowBadge(legacy.canShowBadge())
    if (legacy.lockscreenVisibility != NotificationListenerService.Ranking.VISIBILITY_NO_OVERRIDE) lockscreenVisibility = legacy.lockscreenVisibility
  }

  /** The system settings of one channel (sound, importance, DND override). */
  fun settingsIntent(context: Context, channelId: String): Intent =
    Intent(Settings.ACTION_CHANNEL_NOTIFICATION_SETTINGS)
      .putExtra(Settings.EXTRA_APP_PACKAGE, context.packageName)
      .putExtra(Settings.EXTRA_CHANNEL_ID, channelId)

  /**
   * The system settings of one conversation's notifications (Android 11+: sound, pop-up, priority
   * conversation; the system makes the conversation's own channel when the user changes one);
   * before, the channel its messages post on.
   */
  fun conversationSettingsIntent(context: Context, conversationId: String, isDirect: Boolean): Intent {
    val parent = if (isDirect) DIRECT_MESSAGES else parentFor(context, conversationId)
    val intent = settingsIntent(context, parent)
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) intent.putExtra(Settings.EXTRA_CONVERSATION_ID, conversationId)
    return intent
  }

  /** The app's notification settings (all channels, the master switch). */
  fun appSettingsIntent(context: Context): Intent =
    Intent(Settings.ACTION_APP_NOTIFICATION_SETTINGS).putExtra(Settings.EXTRA_APP_PACKAGE, context.packageName)

  data class Channel(
    val id: String,
    @StringRes val name: Int,
    @StringRes val description: Int,
    val importance: Int,
  ) {
    fun build(context: Context): NotificationChannel =
      NotificationChannel(id, context.getString(name), importance).apply {
        description = context.getString(this@Channel.description)
        if (this@Channel.id == INCOMING_CALLS) ringLikeAPhone()
        // Direct messages may float as bubbles when the user opts a conversation in (#591); the
        // user can still turn it off for the channel in the system settings.
        if (this@Channel.id == DIRECT_MESSAGES && Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) setAllowBubbles(true)
      }

    private fun NotificationChannel.ringLikeAPhone() {
      val ringtone = RingtoneManager.getDefaultUri(RingtoneManager.TYPE_RINGTONE)
      val attributes = AudioAttributes.Builder()
        .setUsage(AudioAttributes.USAGE_NOTIFICATION_RINGTONE)
        .setContentType(AudioAttributes.CONTENT_TYPE_SONIFICATION)
        .build()
      setSound(ringtone, attributes)
      enableVibration(true)
      vibrationPattern = RING_VIBRATION
      lockscreenVisibility = Notification.VISIBILITY_PUBLIC
    }
  }
}
