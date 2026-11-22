package de.zeitgleich.teamcraft.notifications

import android.app.NotificationChannel
import android.app.NotificationManager
import android.content.Context
import android.os.Build
import android.provider.Settings

/**
 * How the system is set up to show the app's notifications, for the diagnostics export: where the
 * app was installed from, and every notification channel with its importance and sound, including
 * the conversations the user adjusted in the system settings (their own channels). Conversations
 * are named by ID only, never by their names; nothing here is content.
 */
object NotificationSetup {
  /** One channel as the export lists it. */
  data class ChannelLine(
    val id: String,
    val importance: Int,
    val hasDefaultSound: Boolean,
    val hasSound: Boolean,
    val vibrates: Boolean,
    val maybeParentId: String? = null,
    val maybeConversationId: String? = null,
    val isImportantConversation: Boolean = false,
    val isDemoted: Boolean = false,
    val hasUserImportance: Boolean = false,
  )

  /** The export's lines: the installer, then each channel. */
  fun describe(context: Context): String {
    val manager = context.getSystemService(NotificationManager::class.java)
    val channels = runCatching { manager?.notificationChannels.orEmpty() }.getOrDefault(emptyList()).map(::line)
    return format(installer(context), channels)
  }

  /** Pure, unit-tested: the lines of [describe]. */
  fun format(installer: String, channels: List<ChannelLine>): String = buildString {
    append("Installed from: ").append(installer).append('\n')
    append("Notification channels:")
    if (channels.isEmpty()) append(" none")
    append('\n')
    channels.sortedWith(compareBy({ it.maybeParentId ?: it.id }, { it.maybeConversationId.orEmpty() })).forEach { channel ->
      append("  ")
      if (channel.maybeConversationId != null) {
        append(channel.maybeParentId).append(" / conversation ").append(channel.maybeConversationId)
      } else {
        append(channel.id)
      }
      append(": ").append(importanceName(channel.importance))
      if (channel.hasUserImportance) append(" (set by user)")
      val sound = when {
        !channel.hasSound -> "none"
        channel.hasDefaultSound -> "default"
        else -> "custom"
      }
      append(", sound ").append(sound)
      append(", vibration ").append(if (channel.vibrates) "on" else "off")
      if (channel.isImportantConversation) append(", priority conversation")
      if (channel.isDemoted) append(", not shown as a conversation")
      append('\n')
    }
  }

  fun importanceName(importance: Int): String = when (importance) {
    NotificationManager.IMPORTANCE_NONE -> "off"
    NotificationManager.IMPORTANCE_MIN -> "min"
    NotificationManager.IMPORTANCE_LOW -> "low (silent)"
    NotificationManager.IMPORTANCE_DEFAULT -> "default (sound, no pop-up)"
    NotificationManager.IMPORTANCE_HIGH -> "high (sound and pop-up)"
    else -> "unspecified ($importance)"
  }

  private fun line(channel: NotificationChannel): ChannelLine {
    val isConversation = Build.VERSION.SDK_INT >= Build.VERSION_CODES.R && channel.conversationId != null
    return ChannelLine(
      id = channel.id,
      importance = channel.importance,
      hasDefaultSound = channel.sound == Settings.System.DEFAULT_NOTIFICATION_URI,
      hasSound = channel.sound != null,
      vibrates = channel.shouldVibrate(),
      maybeParentId = if (isConversation) channel.parentChannelId else null,
      maybeConversationId = if (isConversation) channel.conversationId else null,
      isImportantConversation = isConversation && channel.isImportantConversation,
      isDemoted = Build.VERSION.SDK_INT >= Build.VERSION_CODES.S && channel.isDemoted,
      hasUserImportance = Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q && channel.hasUserSetImportance(),
    )
  }

  /** The store or tool that installed the app (`getInstallSourceInfo`), or "unknown". */
  private fun installer(context: Context): String = runCatching {
    val packages = context.packageManager
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
      val info = packages.getInstallSourceInfo(context.packageName)
      val installing = info.installingPackageName ?: "unknown"
      val initiating = info.initiatingPackageName
      if (initiating != null && initiating != installing) "$installing (started by $initiating)" else installing
    } else {
      @Suppress("DEPRECATION")
      packages.getInstallerPackageName(context.packageName) ?: "unknown"
    }
  }.getOrDefault("unknown")
}
