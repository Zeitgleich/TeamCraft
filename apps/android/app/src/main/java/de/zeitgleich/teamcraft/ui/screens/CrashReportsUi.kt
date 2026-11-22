package de.zeitgleich.teamcraft.ui.screens

import androidx.annotation.StringRes
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Info
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Icon
import androidx.compose.material3.ListItemShapes
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.platform.LocalAccessibilityManager
import androidx.compose.ui.res.stringResource
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.crash.CrashReports
import de.zeitgleich.teamcraft.crash.CrashReportsStatus
import kotlinx.coroutines.delay
import uniffi.teamcraft_ffi.crashNoticeTimeoutMs

/**
 * Settings → Privacy and security: "Send crash reports", only while the instance has them on
 * (docs/crash-reports.md, "Settings"); the page leaves the row out while [CrashReports.status] is
 * unknown. Mandatory on the instance: on, disabled, and it says so.
 */
@Composable
fun CrashReportsRow(shapes: ListItemShapes, crashReports: CrashReports, status: CrashReportsStatus) {
  var isAllowed by remember { mutableStateOf(crashReports.isAllowedByUser) }
  SettingsSwitchRow(
    shapes,
    stringResource(R.string.crash_reports_send),
    isOn = isAllowed || !status.allowsOptOut,
    maybeSummary = stringResource(hintText(status)),
    isEnabled = status.allowsOptOut,
  ) {
    isAllowed = it
    crashReports.setAllowedByUser(it)
  }
}

/** Once, on the first start with crash reports on, when the instance wants people told. */
@Composable
fun CrashReportsNotice(crashReports: CrashReports, onSettings: () -> Unit) {
  val maybeStatus by crashReports.status.collectAsState()
  var isDismissed by remember { mutableStateOf(false) }
  val status = maybeStatus
  if (status == null || isDismissed || !crashReports.isNoticeDue) return
  val close = {
    crashReports.markNoticeSeen()
    isDismissed = true
  }
  // Goes away by itself and counts as seen; the system's "time to take action" setting stretches
  // it for people who need longer (WCAG 2.2.1), and the Settings page keeps the switch.
  val accessibility = LocalAccessibilityManager.current
  LaunchedEffect(Unit) {
    val base = crashNoticeTimeoutMs().toLong()
    delay(accessibility?.calculateRecommendedTimeoutMillis(base, containsIcons = true, containsText = true, containsControls = status.allowsOptOut) ?: base)
    close()
  }
  AlertDialog(
    onDismissRequest = close,
    icon = { Icon(Icons.Outlined.Info, contentDescription = null) },
    title = { Text(stringResource(R.string.crash_reports_title)) },
    text = { Text(stringResource(noticeText(status))) },
    confirmButton = { TextButton(onClick = close) { Text(stringResource(R.string.ok)) } },
    // Only an instance that lets people opt out has a switch to point to.
    dismissButton = if (status.allowsOptOut) {
      {
        TextButton(onClick = {
          close()
          onSettings()
        }) { Text(stringResource(R.string.crash_reports_notice_settings)) }
      }
    } else {
      null
    },
  )
}

/** Who receives the reports and whether people may opt out choose the notice's words. */
@StringRes
internal fun noticeText(status: CrashReportsStatus): Int = when {
  status.isPublisherRecipient && status.allowsOptOut -> R.string.crash_reports_notice_publisher_optional
  status.isPublisherRecipient -> R.string.crash_reports_notice_publisher_required
  status.allowsOptOut -> R.string.crash_reports_notice_organization_optional
  else -> R.string.crash_reports_notice_organization_required
}

/** The privacy settings' hint under the switch, by the same two facts. */
@StringRes
internal fun hintText(status: CrashReportsStatus): Int = when {
  status.isPublisherRecipient && status.allowsOptOut -> R.string.crash_reports_send_hint_publisher
  status.isPublisherRecipient -> R.string.crash_reports_required_hint_publisher
  status.allowsOptOut -> R.string.crash_reports_send_hint_organization
  else -> R.string.crash_reports_required_hint_organization
}
