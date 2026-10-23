package de.zeitgleich.teamcraft.ui.screens

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.os.Build
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Notifications
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import de.zeitgleich.teamcraft.R

/**
 * Asks for the notification permission once the account is set up, with a line on why, instead of
 * the bare system dialog at the first start (before the user even signed in). The system dialog
 * only opens from the button; "Not now" is remembered, the setting stays in Settings → Notifications.
 */
@Composable
fun NotificationPermissionCard() {
  val context = LocalContext.current
  val prefs = remember { FirstRunPrefs.prefs(context) }
  var isShown by remember {
    mutableStateOf(shouldOfferNotificationPermission(Build.VERSION.SDK_INT, hasNotificationPermission(context), prefs.hasAnsweredNotificationPrompt))
  }
  val launcher = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) {
    prefs.setAnsweredNotificationPrompt()
    isShown = false
  }
  if (!isShown) return
  Card(Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 4.dp)) {
    Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
      Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
        Icon(Icons.Filled.Notifications, contentDescription = null, tint = MaterialTheme.colorScheme.primary)
        Text(stringResource(R.string.notification_prompt_title), style = MaterialTheme.typography.titleSmall)
      }
      Text(stringResource(R.string.notification_prompt_text), style = MaterialTheme.typography.bodyMedium)
      Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
        TextButton(onClick = {
          prefs.setAnsweredNotificationPrompt()
          isShown = false
        }) { Text(stringResource(R.string.not_now)) }
        Button(onClick = { launcher.launch(Manifest.permission.POST_NOTIFICATIONS) }) {
          Text(stringResource(R.string.notification_prompt_allow))
        }
      }
    }
  }
}

/** Android 13+ asks for the permission; before, notifications are on unless switched off. */
fun shouldOfferNotificationPermission(sdkInt: Int, hasPermission: Boolean, hasAnswered: Boolean): Boolean =
  sdkInt >= Build.VERSION_CODES.TIRAMISU && !hasPermission && !hasAnswered

private fun hasNotificationPermission(context: Context): Boolean =
  Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU ||
    ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED

/** What the first run asked already on this device. */
class FirstRunPrefs private constructor(context: Context) {
  private val prefs = context.getSharedPreferences(FILE, Context.MODE_PRIVATE)

  val hasAnsweredNotificationPrompt: Boolean get() = prefs.getBoolean(KEY_NOTIFICATION_PROMPT, false)

  fun setAnsweredNotificationPrompt() {
    prefs.edit().putBoolean(KEY_NOTIFICATION_PROMPT, true).apply()
  }

  companion object {
    private const val FILE = "first_run"
    private const val KEY_NOTIFICATION_PROMPT = "notification_prompt_answered"

    fun prefs(context: Context) = FirstRunPrefs(context.applicationContext)
  }
}
