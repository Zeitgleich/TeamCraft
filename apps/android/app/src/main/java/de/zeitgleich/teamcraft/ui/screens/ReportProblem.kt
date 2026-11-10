package de.zeitgleich.teamcraft.ui.screens

import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.layout.only
import androidx.compose.foundation.layout.WindowInsetsSides
import androidx.compose.foundation.layout.consumeWindowInsets
import android.content.Context
import android.net.Uri
import android.provider.OpenableColumns
import android.widget.Toast
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.BuildConfig
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.ChatActions
import de.zeitgleich.teamcraft.core.ReportScreenshot
import de.zeitgleich.teamcraft.ui.AppState
import de.zeitgleich.teamcraft.ui.MainViewModel
import de.zeitgleich.teamcraft.ui.forms.FieldErrors
import de.zeitgleich.teamcraft.ui.forms.MAX_REPORT_SCREENSHOT_BYTES
import de.zeitgleich.teamcraft.ui.forms.PickedScreenshotInfo
import de.zeitgleich.teamcraft.ui.forms.REPORT_SCREENSHOT_TYPES
import de.zeitgleich.teamcraft.ui.forms.ReportField
import de.zeitgleich.teamcraft.ui.forms.fieldError
import de.zeitgleich.teamcraft.ui.forms.focusFirst
import de.zeitgleich.teamcraft.ui.forms.reportErrors
import de.zeitgleich.teamcraft.ui.forms.supportingText
import de.zeitgleich.teamcraft.ui.rememberIsOnline
import java.util.UUID
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import uniffi.teamcraft_ffi.bugReportFileName
import uniffi.teamcraft_ffi.bugReportText

/** A picture picked for the report, with what the checks need. */
private class PickedScreenshot(val fileName: String, val info: PickedScreenshotInfo, val maybeBytes: ByteArray?)

/**
 * "Report a problem" (docs/bug-reports.md, #1362): a description, an optional screenshot and the
 * report text the person sees before it is sent, end-to-end encrypted, to the recipients of the
 * verified policy. Offline the report waits on the device and goes out once the app is online.
 */
@Composable
fun ReportProblemScreen(viewModel: MainViewModel, account: AppState.Ready, onBack: () -> Unit) {
  val context = LocalContext.current
  val scope = rememberCoroutineScope()
  val isOnline = rememberIsOnline()
  // One ID and one moment per opened screen, kept over rotation: a retried send is the same report.
  val id = rememberSaveable { UUID.randomUUID().toString().replace("-", "") }
  val createdAtMs = rememberSaveable { System.currentTimeMillis() }
  val reportText = remember {
    bugReportText(
      platform = "android",
      appVersion = BuildConfig.VERSION_NAME,
      osVersion = osVersionLine(),
      userId = account.userId,
      connection = if (isOnline) "online" else "offline",
      createdAtMs = createdAtMs,
    )
  }
  var description by rememberSaveable { mutableStateOf("") }
  var maybeScreenshot by remember { mutableStateOf<PickedScreenshot?>(null) }
  var errors by remember { mutableStateOf(FieldErrors.of<ReportField>()) }
  var wasRefused by remember { mutableStateOf(false) }
  var isSending by remember { mutableStateOf(false) }
  var maybeFailure by remember { mutableStateOf<Int?>(null) }
  val descriptionFocus = remember { FocusRequester() }
  val screenshotFocus = remember { FocusRequester() }
  fun follow() {
    if (wasRefused) errors = reportErrors(description, maybeScreenshot?.info)
  }
  val picker = rememberLauncherForActivityResult(ActivityResultContracts.PickVisualMedia()) { maybeUri ->
    val uri = maybeUri ?: return@rememberLauncherForActivityResult
    scope.launch {
      maybeScreenshot = withContext(Dispatchers.IO) { readScreenshot(context, uri) }
      follow()
    }
  }
  fun send() {
    val found = reportErrors(description, maybeScreenshot?.info)
    if (!found.isValid) {
      errors = found
      wasRefused = true
      found.focusFirst(mapOf(ReportField.DESCRIPTION to descriptionFocus, ReportField.SCREENSHOT to screenshotFocus))
      return
    }
    val screenshot = maybeScreenshot?.let { picked ->
      picked.maybeBytes?.let { ReportScreenshot(picked.fileName, picked.info.mediaType, it) }
    }
    val action = ChatActions.sendBugReport(id, description.trim(), reportText, bugReportFileName(createdAtMs), screenshot)
    isSending = true
    maybeFailure = null
    scope.launch {
      val maybeError = viewModel.tryAct(action)
      isSending = false
      if (maybeError == null) {
        Toast.makeText(context, R.string.report_queued, Toast.LENGTH_LONG).show()
        onBack()
      } else {
        maybeFailure = maybeError
      }
    }
  }

  Scaffold(
    topBar = { ChromeTopBar(title = stringResource(R.string.report_title), maybeOnBack = onBack) },
    // Edge to edge: the page scrolls behind the navigation bar and ends clear of it (see Settings).
    contentWindowInsets = ScreenInsets.only(WindowInsetsSides.Top + WindowInsetsSides.Horizontal),
  ) { padding ->
    Column(
      Modifier
        .fillMaxSize()
        .padding(padding)
        .consumeWindowInsets(padding)
        // The keyboard shrinks the scroll viewport, so the focused field scrolls into view above it;
        // the navigation bar inset below is then consumed and only applies with the keyboard closed.
        .imePadding()
        .verticalScroll(rememberScrollState())
        .windowInsetsPadding(ScreenInsets.only(WindowInsetsSides.Bottom))
        .padding(16.dp),
      verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
      Text(
        stringResource(R.string.report_intro),
        style = MaterialTheme.typography.bodyMedium,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
      )
      OutlinedTextField(
        value = description,
        onValueChange = {
          description = it
          follow()
        },
        label = { Text(stringResource(R.string.report_description)) },
        minLines = 4,
        keyboardOptions = textKeyboard(),
        isError = errors[ReportField.DESCRIPTION] != null,
        supportingText = supportingText(errors[ReportField.DESCRIPTION], R.string.report_description_hint),
        modifier = Modifier
          .fillMaxWidth()
          .focusRequester(descriptionFocus)
          .fieldError(errors[ReportField.DESCRIPTION]),
      )
      Text(stringResource(R.string.report_screenshot), style = MaterialTheme.typography.titleSmall)
      Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(8.dp),
        modifier = Modifier.fillMaxWidth().fieldError(errors[ReportField.SCREENSHOT]),
      ) {
        val picked = maybeScreenshot
        if (picked == null) {
          OutlinedButton(
            onClick = { picker.launch(PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageOnly)) },
            modifier = Modifier.focusRequester(screenshotFocus),
          ) {
            Icon(painterResource(R.drawable.ic_add_photo_alternate), contentDescription = null)
            Spacer(Modifier.width(8.dp))
            Text(stringResource(R.string.report_screenshot_add))
          }
        } else {
          Text(picked.fileName, maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f))
          TextButton(
            onClick = {
              maybeScreenshot = null
              follow()
            },
            modifier = Modifier.focusRequester(screenshotFocus),
          ) { Text(stringResource(R.string.report_screenshot_remove)) }
        }
      }
      supportingText(errors[ReportField.SCREENSHOT], R.string.report_screenshot_hint)?.invoke()
      Text(stringResource(R.string.report_preview), style = MaterialTheme.typography.titleSmall)
      Text(
        stringResource(R.string.report_preview_hint),
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
      )
      Surface(
        color = MaterialTheme.colorScheme.surfaceContainerHigh,
        shape = MaterialTheme.shapes.small,
        modifier = Modifier.fillMaxWidth().heightIn(max = 320.dp),
      ) {
        Text(
          reportText,
          style = MaterialTheme.typography.bodySmall,
          fontFamily = FontFamily.Monospace,
          softWrap = false,
          modifier = Modifier
            .verticalScroll(rememberScrollState())
            .horizontalScroll(rememberScrollState())
            .padding(12.dp),
        )
      }
      maybeFailure?.let { failure ->
        Text(
          stringResource(failure),
          color = MaterialTheme.colorScheme.error,
          modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
        )
      }
      Button(onClick = ::send, enabled = !isSending, modifier = Modifier.fillMaxWidth()) {
        Text(stringResource(R.string.report_send))
      }
    }
  }
}

/** Reads the picked picture's name, type and size; its bytes only when it may be sent. */
private fun readScreenshot(context: Context, uri: Uri): PickedScreenshot {
  val resolver = context.contentResolver
  val mediaType = resolver.getType(uri).orEmpty()
  var fileName = "screenshot"
  var sizeBytes = -1L
  resolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE), null, null, null)?.use { cursor ->
    if (cursor.moveToFirst()) {
      cursor.getString(0)?.let { fileName = it }
      if (!cursor.isNull(1)) sizeBytes = cursor.getLong(1)
    }
  }
  val isAllowed = mediaType in REPORT_SCREENSHOT_TYPES && sizeBytes in 0..MAX_REPORT_SCREENSHOT_BYTES
  // At most one byte over the limit is read: a provider that lied about the size can't fill memory.
  val maybeBytes = if (isAllowed) {
    runCatching {
      resolver.openInputStream(uri)?.use { input -> input.readNBytesCompat(MAX_REPORT_SCREENSHOT_BYTES.toInt() + 1) }
    }.getOrNull()?.takeIf { it.size <= MAX_REPORT_SCREENSHOT_BYTES }
  } else {
    null
  }
  val size = maybeBytes?.size?.toLong() ?: sizeBytes.coerceAtLeast(0)
  return PickedScreenshot(fileName, PickedScreenshotInfo(mediaType, size, isRead = maybeBytes != null), maybeBytes)
}

/** Up to [limit] bytes of the stream (`readNBytes` needs API 33). */
private fun java.io.InputStream.readNBytesCompat(limit: Int): ByteArray {
  val out = java.io.ByteArrayOutputStream()
  val buffer = ByteArray(64 * 1024)
  while (out.size() < limit) {
    val read = read(buffer, 0, minOf(buffer.size, limit - out.size()))
    if (read < 0) break
    out.write(buffer, 0, read)
  }
  return out.toByteArray()
}
