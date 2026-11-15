package de.zeitgleich.teamcraft.ui.screens

import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.provider.ContactsContract
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.LocalTextStyle
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.TextRange
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.ui.Detected
import de.zeitgleich.teamcraft.ui.DetectedKind
import de.zeitgleich.teamcraft.ui.copyText

/** How many links, numbers and addresses of one message the action sheet lists. */
private const val MAX_DETECTED = 3

/** The longest part of a value the sheet's rows show; the whole value is what is opened or copied. */
private const val MAX_SHOWN = 32

/**
 * Rows for the links, phone numbers, e-mail and postal addresses of a message (#2047, #2048):
 * open (or call, write, show on the map), copy, and share or add to contacts where that fits.
 */
@Composable
internal fun DetectedRows(items: List<Detected>, onDone: () -> Unit) {
  val context = LocalContext.current
  for (item in items.distinctBy { it.value }.take(MAX_DETECTED)) {
    val shown = shortened(item.value)
    val open = when (item.kind) {
      DetectedKind.WEB -> R.string.detected_open_web
      DetectedKind.PHONE -> R.string.detected_call
      DetectedKind.EMAIL -> R.string.detected_write
      DetectedKind.ADDRESS -> R.string.detected_map
    }
    DetectedRow(stringResource(open, shown)) {
      startDetected(context, item)
      onDone()
    }
    DetectedRow(stringResource(R.string.detected_copy, shown)) {
      copyText(context, item.value)
      onDone()
    }
    when (item.kind) {
      DetectedKind.WEB -> DetectedRow(stringResource(R.string.detected_share, shown)) {
        shareText(context, item.value)
        onDone()
      }
      DetectedKind.PHONE, DetectedKind.EMAIL -> DetectedRow(stringResource(R.string.detected_add_contact, shown)) {
        addToContacts(context, item)
        onDone()
      }
      DetectedKind.ADDRESS -> Unit
    }
  }
}

@Composable
private fun DetectedRow(label: String, onClick: () -> Unit) {
  Text(label, modifier = Modifier.fillMaxWidth().clickable(onClick = onClick).padding(horizontal = 24.dp, vertical = 14.dp), maxLines = 1)
}

/**
 * The message's text in a field that can't be edited, so the user can select words (the handles and
 * the system's own menu work as in any text field) and quote just those (#2050). Nothing selected
 * quotes the whole text.
 */
@Composable
internal fun SelectTextDialog(body: String, onQuote: (String) -> Unit, onDismiss: () -> Unit) {
  var value by remember { mutableStateOf(TextFieldValue(body)) }
  AlertDialog(
    onDismissRequest = onDismiss,
    title = { Text(stringResource(R.string.select_text_title)) },
    text = {
      Column {
        Text(stringResource(R.string.select_text_hint), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        BasicTextField(
          value = value,
          onValueChange = { next -> value = next.copy(text = body) },
          readOnly = true,
          textStyle = LocalTextStyle.current.copy(color = MaterialTheme.colorScheme.onSurface),
          modifier = Modifier.fillMaxWidth().padding(top = 12.dp),
        )
      }
    },
    confirmButton = {
      TextButton(onClick = { onQuote(selectedOrAll(body, value.selection)) }) { Text(stringResource(R.string.select_text_quote)) }
    },
    dismissButton = { TextButton(onClick = onDismiss) { Text(stringResource(R.string.cancel)) } },
  )
}

/** The selected part of [body], trimmed; all of it when nothing (or only blanks) is selected. */
internal fun selectedOrAll(body: String, selection: TextRange): String {
  val start = selection.min.coerceIn(0, body.length)
  val end = selection.max.coerceIn(0, body.length)
  return body.substring(start, end).trim().ifEmpty { body }
}

internal fun shortened(value: String): String = if (value.length <= MAX_SHOWN) value else value.take(MAX_SHOWN - 1) + "…"

internal fun startDetected(context: Context, item: Detected) {
  if (item.kind == DetectedKind.WEB) {
    openLink(context, item.value)
    return
  }
  launch(context, Intent(Intent.ACTION_VIEW, Uri.parse(item.uri)))
}

private fun shareText(context: Context, text: String) {
  val send = Intent(Intent.ACTION_SEND).setType("text/plain").putExtra(Intent.EXTRA_TEXT, text)
  launch(context, Intent.createChooser(send, null))
}

private fun addToContacts(context: Context, item: Detected) {
  val insert = Intent(ContactsContract.Intents.Insert.ACTION).setType(ContactsContract.RawContacts.CONTENT_TYPE)
    .putExtra(if (item.kind == DetectedKind.PHONE) ContactsContract.Intents.Insert.PHONE else ContactsContract.Intents.Insert.EMAIL, item.value)
  launch(context, insert)
}

private fun launch(context: Context, intent: Intent) {
  try {
    context.startActivity(intent)
  } catch (failure: ActivityNotFoundException) {
    // No app handles it (a tablet without a dialer); there is nothing sensible to do.
  } catch (failure: SecurityException) {
    // The handling app refused to be started from here.
  }
}
