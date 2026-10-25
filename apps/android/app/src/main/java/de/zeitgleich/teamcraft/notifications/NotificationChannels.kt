package de.zeitgleich.teamcraft.notifications

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.content.Context
import android.content.Intent
import android.media.AudioAttributes
import android.media.RingtoneManager
import android.provider.Settings
import androidx.annotation.StringRes
import de.zeitgleich.teamcraft.R

/**
 * The app's notification channels (notifications.md, "Android channels"). Importance can't change
 * after a channel was created (the user owns it then), so a channel that needs another importance
 * gets a new ID: incoming calls ring on [INCOMING_CALLS], the ongoing call stays on [CALLS].
 */
object NotificationChannels {
  const val DIRECT_MESSAGES = "direct_messages"
  const val MESSAGES = "messages"
  const val MENTIONS = "mentions"
  const val INCOMING_CALLS = "incoming_calls"
  const val CALLS = "calls"
  const val MEETINGS = "meetings"
  const val OTHER = "other"

  private val RING_VIBRATION = longArrayOf(0, 1_000, 1_000)

  /** What the diagnostics list, in the order they show them. */
  val all: List<Channel> = listOf(
    Channel(DIRECT_MESSAGES, R.string.notification_channel_direct_messages, R.string.notification_channel_direct_messages_description, NotificationManager.IMPORTANCE_HIGH),
    Channel(MESSAGES, R.string.notification_channel_messages, R.string.notification_channel_messages_description, NotificationManager.IMPORTANCE_DEFAULT),
    Channel(MENTIONS, R.string.notification_channel_mentions, R.string.notification_channel_mentions_description, NotificationManager.IMPORTANCE_HIGH),
    Channel(INCOMING_CALLS, R.string.notification_channel_incoming_calls, R.string.notification_channel_incoming_calls_description, NotificationManager.IMPORTANCE_HIGH),
    Channel(CALLS, R.string.call_channel, R.string.notification_channel_calls_description, NotificationManager.IMPORTANCE_DEFAULT),
    Channel(MEETINGS, R.string.notification_channel_meetings, R.string.notification_channel_meetings_description, NotificationManager.IMPORTANCE_HIGH),
    Channel(OTHER, R.string.notification_channel_other, R.string.notification_channel_other_description, NotificationManager.IMPORTANCE_LOW),
  )

  /**
   * Creates the channels, or updates their names and descriptions (the language may have changed;
   * importance and sound stay as the user set them).
   */
  fun create(context: Context) {
    val manager = context.getSystemService(NotificationManager::class.java) ?: return
    manager.createNotificationChannels(all.map { it.build(context) })
  }

  /** The system settings of one channel (sound, importance, DND override). */
  fun settingsIntent(context: Context, channelId: String): Intent =
    Intent(Settings.ACTION_CHANNEL_NOTIFICATION_SETTINGS)
      .putExtra(Settings.EXTRA_APP_PACKAGE, context.packageName)
      .putExtra(Settings.EXTRA_CHANNEL_ID, channelId)

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
