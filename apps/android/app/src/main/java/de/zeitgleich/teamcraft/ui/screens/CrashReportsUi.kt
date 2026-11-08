package de.zeitgleich.teamcraft.ui.screens

import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Info
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.res.stringResource
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.crash.CrashReports

/**
 * Settings → Privacy and security: "Send crash reports", only while the instance has them on
 * (docs/crash-reports.md, "Settings"). Mandatory on the instance: on, disabled, and it says so.
 */
@Composable
fun CrashReportsSetting(crashReports: CrashReports) {
  val maybeStatus by crashReports.status.collectAsState()
  val status = maybeStatus ?: return
  var isAllowed by remember { mutableStateOf(crashReports.isAllowedByUser) }
  SettingSwitch(
    R.string.crash_reports_send,
    isOn = isAllowed || !status.allowsOptOut,
    isEnabled = status.allowsOptOut,
  ) {
    isAllowed = it
    crashReports.setAllowedByUser(it)
  }
  Text(
    stringResource(if (status.allowsOptOut) R.string.crash_reports_send_hint else R.string.crash_reports_required_hint),
    style = MaterialTheme.typography.bodySmall,
  )
}

/** Once, on the first start with crash reports on, when the instance wants people told. */
@Composable
fun CrashReportsNotice(crashReports: CrashReports, onSettings: () -> Unit) {
  val maybeStatus by crashReports.status.collectAsState()
  var isDismissed by remember { mutableStateOf(false) }
  if (maybeStatus == null || isDismissed || !crashReports.isNoticeDue) return
  val close = {
    crashReports.markNoticeSeen()
    isDismissed = true
  }
  AlertDialog(
    onDismissRequest = close,
    icon = { Icon(Icons.Outlined.Info, contentDescription = null) },
    title = { Text(stringResource(R.string.crash_reports_title)) },
    text = { Text(stringResource(R.string.crash_reports_notice)) },
    confirmButton = { TextButton(onClick = close) { Text(stringResource(R.string.ok)) } },
    dismissButton = {
      TextButton(onClick = {
        close()
        onSettings()
      }) { Text(stringResource(R.string.crash_reports_notice_settings)) }
    },
  )
}
