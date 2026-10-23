package de.zeitgleich.teamcraft.notifications

import android.Manifest
import android.app.ActivityManager
import android.app.NotificationManager
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.PowerManager
import android.provider.Settings
import androidx.core.app.NotificationManagerCompat
import androidx.core.content.ContextCompat
import de.zeitgleich.teamcraft.calls.IncomingCallNotifier
import de.zeitgleich.teamcraft.push.UnifiedPushBackend

/**
 * What the system decides about the app's notifications, read fresh for the diagnostics: the
 * permission, the app and channel switches, full-screen rings, battery optimisation and the
 * phone's own do-not-disturb.
 */
data class NotificationHealth(
  val hasPermission: Boolean,
  val areEnabled: Boolean,
  val channels: List<ChannelState>,
  val canUseFullScreenIntent: Boolean,
  val isAppExemptFromBatteryOptimization: Boolean,
  val isBackgroundRestricted: Boolean,
  /** The phone's own DND lets only some or no notifications through. */
  val isSystemDndOn: Boolean,
  val distributors: List<Distributor>,
  val maybeCurrentDistributor: Distributor?,
) {
  data class ChannelState(val channel: NotificationChannels.Channel, val importance: Int)

  /** An installed UnifiedPush distributor and whether the system lets it run in the background. */
  data class Distributor(val packageName: String, val label: String, val isExemptFromBatteryOptimization: Boolean)

  companion object {
    fun read(context: Context): NotificationHealth {
      val system = context.getSystemService(NotificationManager::class.java)
      val power = context.getSystemService(PowerManager::class.java)
      val distributors = UnifiedPushBackend.distributors(context).map { distributor(context, power, it) }
      val maybeCurrent = UnifiedPushBackend.currentDistributor(context)
      return NotificationHealth(
        hasPermission = hasPermission(context),
        areEnabled = NotificationManagerCompat.from(context).areNotificationsEnabled(),
        channels = NotificationChannels.all.map { channel ->
          ChannelState(channel, system?.getNotificationChannel(channel.id)?.importance ?: NotificationManager.IMPORTANCE_NONE)
        },
        canUseFullScreenIntent = IncomingCallNotifier.canUseFullScreenIntent(context),
        isAppExemptFromBatteryOptimization = power?.isIgnoringBatteryOptimizations(context.packageName) == true,
        isBackgroundRestricted = Build.VERSION.SDK_INT >= Build.VERSION_CODES.P &&
          context.getSystemService(ActivityManager::class.java)?.isBackgroundRestricted == true,
        isSystemDndOn = system?.currentInterruptionFilter.let {
          it != null && it != NotificationManager.INTERRUPTION_FILTER_ALL && it != NotificationManager.INTERRUPTION_FILTER_UNKNOWN
        },
        distributors = distributors,
        maybeCurrentDistributor = distributors.firstOrNull { it.packageName == maybeCurrent },
      )
    }

    /** Android 14+: where the user allows full-screen rings again. */
    fun fullScreenIntentSettings(context: Context): Intent =
      if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
        Intent(Settings.ACTION_MANAGE_APP_USE_FULL_SCREEN_INTENT, Uri.fromParts("package", context.packageName, null))
      } else {
        NotificationChannels.appSettingsIntent(context)
      }

    /** An app's info page, where its battery use is set (the distributor's, on UnifiedPush). */
    fun appInfo(packageName: String): Intent =
      Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS, Uri.fromParts("package", packageName, null))

    private fun hasPermission(context: Context): Boolean =
      Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU ||
        ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED

    private fun distributor(context: Context, power: PowerManager?, packageName: String): Distributor {
      val label = runCatching {
        val info = context.packageManager.getApplicationInfo(packageName, 0)
        context.packageManager.getApplicationLabel(info).toString()
      }.getOrDefault(packageName)
      return Distributor(packageName, label, power?.isIgnoringBatteryOptimizations(packageName) == true)
    }
  }
}
