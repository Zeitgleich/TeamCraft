package de.zeitgleich.teamcraft.ui

import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.res.stringResource
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

/** The question a link asks before anything happens: never opened silently, always naming the host. */
@Composable
fun AppLinkQuestion(link: OpenedAppLink, onSignIn: (String) -> Unit, onBrowser: () -> Unit, onCancel: () -> Unit) {
  val step = link.step
  val (text, maybeSignInHost) = when (step) {
    is OpenedAppLinkStep.AskForeignServer ->
      if (step.canAddAccount) {
        stringResource(R.string.app_link_foreign_add, step.host) to step.host
      } else {
        stringResource(R.string.app_link_foreign_other, step.host) to null
      }
    is OpenedAppLinkStep.AskWebOnly -> stringResource(R.string.app_link_web_only, step.host) to null
    OpenedAppLinkStep.Navigate -> return
  }
  AlertDialog(
    onDismissRequest = onCancel,
    title = { Text(stringResource(R.string.app_link_title)) },
    text = { Text(text) },
    confirmButton = {
      if (maybeSignInHost != null) {
        TextButton(onClick = { onSignIn(maybeSignInHost) }) { Text(stringResource(R.string.app_link_sign_in)) }
      }
      TextButton(onClick = onBrowser) { Text(stringResource(R.string.app_link_open_browser)) }
    },
    dismissButton = { TextButton(onClick = onCancel) { Text(stringResource(android.R.string.cancel)) } },
  )
}

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
