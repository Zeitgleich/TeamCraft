package de.zeitgleich.teamcraft.ui

import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import androidx.annotation.StringRes
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import androidx.core.net.toUri
import de.zeitgleich.teamcraft.R
import uniffi.teamcraft_ffi.OpenedAppLink
import uniffi.teamcraft_ffi.OpenedAppLinkStep

/**
 * The link of a view intent, or nothing. Any app can send one, so this only takes the text out; the
 * core decides what it means (`openAppLink`, docs/app-links.md). Never logged: it can hold a key.
 */
fun appLinkOf(intent: Intent): String? = intent.takeIf { it.action == Intent.ACTION_VIEW }?.dataString

/**
 * The link in the browser, not in this app: a plain view intent of a verified host would come right
 * back here. A failure is for the caller to show.
 */
fun openInBrowser(context: Context, url: String): Boolean {
  val intent = Intent(Intent.ACTION_VIEW, url.toUri()).apply {
    selector = Intent.makeMainSelectorActivity(Intent.ACTION_MAIN, Intent.CATEGORY_APP_BROWSER)
    addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
  }
  return try {
    context.startActivity(intent)
    true
  } catch (_: ActivityNotFoundException) {
    false
  }
}

/**
 * The question a link asks before anything happens: never opened silently. The end of the host that
 * says who owns it comes first and large, then the whole host, wrapped, so a long name can't hide it.
 */
@Composable
fun AppLinkQuestion(link: OpenedAppLink, onSignIn: (String) -> Unit, onBrowser: () -> Unit, onCancel: () -> Unit) {
  val step = link.step
  val question = when (step) {
    is OpenedAppLinkStep.AskForeignServer -> HostQuestion(
      if (step.canAddAccount) R.string.app_link_foreign_add else R.string.app_link_foreign_other,
      step.host,
      step.registrableDomain,
      step.host.takeIf { step.canAddAccount },
    )
    is OpenedAppLinkStep.AskWebOnly -> HostQuestion(R.string.app_link_web_only, step.host, step.registrableDomain, null)
    OpenedAppLinkStep.Navigate -> return
  }
  AlertDialog(
    onDismissRequest = onCancel,
    title = { Text(stringResource(R.string.app_link_title)) },
    text = {
      Column(Modifier.verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text(stringResource(question.text))
        Text(question.domain, style = MaterialTheme.typography.titleLarge)
        Text(
          question.host,
          style = MaterialTheme.typography.bodySmall,
          fontFamily = FontFamily.Monospace,
          softWrap = true,
        )
      }
    },
    confirmButton = {
      question.maybeSignInHost?.let { host ->
        TextButton(onClick = { onSignIn(host) }) { Text(stringResource(R.string.app_link_sign_in)) }
      }
      TextButton(onClick = onBrowser) { Text(stringResource(R.string.app_link_open_browser)) }
    },
    dismissButton = { TextButton(onClick = onCancel) { Text(stringResource(android.R.string.cancel)) } },
  )
}

private class HostQuestion(@StringRes val text: Int, val host: String, val domain: String, val maybeSignInHost: String?)

/**
 * A meeting link: the app shows the user's meetings, but has no lobby to knock at for one they
 * aren't invited to yet, so it says so instead of pretending.
 */
@Composable
fun MeetingLinkQuestion(onMeetings: () -> Unit, onBrowser: () -> Unit, onCancel: () -> Unit) {
  AlertDialog(
    onDismissRequest = onCancel,
    title = { Text(stringResource(R.string.app_link_meeting_title)) },
    text = { Text(stringResource(R.string.app_link_meeting)) },
    confirmButton = {
      TextButton(onClick = onBrowser) { Text(stringResource(R.string.app_link_open_browser)) }
      TextButton(onClick = onMeetings) { Text(stringResource(R.string.app_link_my_meetings)) }
    },
    dismissButton = { TextButton(onClick = onCancel) { Text(stringResource(android.R.string.cancel)) } },
  )
}
