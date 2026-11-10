package de.zeitgleich.teamcraft.ui.screens

import android.content.Context
import android.net.Uri
import android.text.format.DateFormat
import android.util.Log
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.ui.theme.Spacing
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import uniffi.teamcraft_ffi.RecoveryKitDraft
import uniffi.teamcraft_ffi.RecoveryKitTexts
import java.util.Date

/** A kit is a few kilobytes; a bigger file isn't read (the core doesn't search it either). */
private const val MAX_KIT_FILE_BYTES = 1024 * 1024
private const val TAG = "TeamCraft"
private const val KIT_MIME_TYPE = "text/html"

/** The key in large monospace groups that wrap as whole groups; TalkBack reads it group by group. */
@OptIn(ExperimentalLayoutApi::class)
@Composable
internal fun KitKeyCard(groups: List<String>) {
  val label = stringResource(R.string.kit_key_label)
  Surface(color = MaterialTheme.colorScheme.surfaceContainerHigh, shape = MaterialTheme.shapes.large, modifier = Modifier.fillMaxWidth()) {
    // Not selectable: the key stays out of clipboard histories (save the kit or write it down).
    FlowRow(
      modifier = Modifier
        .padding(Spacing.l)
        .semantics { contentDescription = "$label: ${groups.joinToString(", ") { it.toCharArray().joinToString(" ") }}" },
      horizontalArrangement = Arrangement.spacedBy(Spacing.l),
      verticalArrangement = Arrangement.spacedBy(Spacing.s),
    ) {
      groups.forEach { group ->
        Text(group, style = MaterialTheme.typography.titleLarge, fontFamily = FontFamily.Monospace, fontWeight = FontWeight.SemiBold)
      }
    }
  }
}

/**
 * "Save recovery kit": the system's file picker (Files, Drive, a password manager's provider) asks
 * where, then the page is written there. [onSaved] gets whether it worked; a cancelled picker is
 * neither.
 */
@Composable
internal fun rememberKitSaver(kit: RecoveryKitDraft, serverHost: String, onSaved: (Boolean) -> Unit): () -> Unit {
  val context = LocalContext.current
  val scope = rememberCoroutineScope()
  val launcher = rememberLauncherForActivityResult(ActivityResultContracts.CreateDocument(KIT_MIME_TYPE)) { maybeUri ->
    val uri = maybeUri ?: return@rememberLauncherForActivityResult
    scope.launch { onSaved(writeKit(context, uri, kit.kitHtml(recoveryKitTexts(context, serverHost)))) }
  }
  return { launcher.launch(uniffi.teamcraft_ffi.recoveryKitFileName(serverHost)) }
}

/** "Open recovery kit…": the system's file picker, then the file's text for [onText] (`null`: unreadable). */
@Composable
internal fun rememberKitOpener(onText: (String?) -> Unit): () -> Unit {
  val context = LocalContext.current
  val scope = rememberCoroutineScope()
  val launcher = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { maybeUri ->
    val uri = maybeUri ?: return@rememberLauncherForActivityResult
    scope.launch { onText(readKit(context, uri)) }
  }
  return { launcher.launch(arrayOf(KIT_MIME_TYPE, "text/plain", "application/octet-stream")) }
}

private suspend fun writeKit(context: Context, uri: Uri, html: String): Boolean = withContext(Dispatchers.IO) {
  try {
    context.contentResolver.openOutputStream(uri, "wt")?.use { it.write(html.toByteArray(Charsets.UTF_8)) } != null
  } catch (failure: Exception) {
    // The provider refused (full, gone offline); the user picks another place or writes it down.
    Log.w(TAG, "could not save the recovery kit", failure)
    false
  }
}

private suspend fun readKit(context: Context, uri: Uri): String? = withContext(Dispatchers.IO) {
  try {
    context.contentResolver.openInputStream(uri)?.use { stream ->
      val bytes = stream.readNBytesCompat(MAX_KIT_FILE_BYTES + 1)
      if (bytes.size > MAX_KIT_FILE_BYTES) "" else bytes.toString(Charsets.UTF_8)
    }
  } catch (failure: Exception) {
    Log.w(TAG, "could not read the picked file", failure)
    null
  }
}

/** At most [limit] bytes (InputStream.readNBytes needs API 33). */
private fun java.io.InputStream.readNBytesCompat(limit: Int): ByteArray {
  val out = java.io.ByteArrayOutputStream()
  val buffer = ByteArray(8 * 1024)
  while (out.size() < limit) {
    val read = read(buffer, 0, minOf(buffer.size, limit - out.size()))
    if (read < 0) break
    out.write(buffer, 0, read)
  }
  return out.toByteArray()
}

/** The kit page's words in the app's language (the same as the web's kit, e2ee.md §6.5.3). */
private fun recoveryKitTexts(context: Context, serverHost: String): RecoveryKitTexts {
  val date = DateFormat.getLongDateFormat(context).format(Date())
  return RecoveryKitTexts(
    language = context.resources.configuration.locales[0].toLanguageTag(),
    title = context.getString(R.string.kit_file_title),
    accountLine = context.getString(R.string.kit_file_account, serverHost, date),
    intro = context.getString(R.string.kit_file_intro),
    keyLabel = context.getString(R.string.kit_key_label),
    keepTitle = context.getString(R.string.kit_file_keep_title),
    keep = listOf(
      context.getString(R.string.kit_file_keep_print),
      context.getString(R.string.kit_file_keep_share),
      context.getString(R.string.kit_file_keep_anyone),
    ),
    useTitle = context.getString(R.string.kit_file_use_title),
    useText = context.getString(R.string.kit_file_use),
    lostTitle = context.getString(R.string.kit_file_lost_title),
    lostText = context.getString(R.string.kit_file_lost),
    neverAsked = context.getString(R.string.kit_file_never),
  )
}
