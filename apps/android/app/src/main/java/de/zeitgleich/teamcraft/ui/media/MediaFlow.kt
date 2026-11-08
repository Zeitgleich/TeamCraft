package de.zeitgleich.teamcraft.ui.media

import android.content.ContentValues
import android.content.Context
import android.net.Uri
import android.os.Build
import android.os.Environment
import android.provider.MediaStore
import android.provider.OpenableColumns
import android.widget.Toast
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.SideEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.Saver
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.luminance
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.compose.ui.window.DialogWindowProvider
import androidx.core.view.WindowCompat
import java.io.File
import java.util.UUID
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.PickedFile
import de.zeitgleich.teamcraft.ui.MainViewModel
import de.zeitgleich.teamcraft.ui.MediaPrefs
import de.zeitgleich.teamcraft.ui.RecentMediaItem

/** The draft is written this long after the last change: one write per gesture, not per frame. */
private const val SAVE_DELAY_MS = 300L

/**
 * How the flow was opened: where it starts and with which items (from the attach sheet's strip or
 * the system picker). Saved with the composer, so the flow reopens after the app was killed.
 */
@Serializable
data class MediaFlowLaunch(val id: String, val stage: MediaStage, val uris: List<String> = emptyList(), val videoUris: List<String> = emptyList()) {
  companion object {
    fun camera() = MediaFlowLaunch(UUID.randomUUID().toString(), MediaStage.Camera)

    fun gallery() = MediaFlowLaunch(UUID.randomUUID().toString(), MediaStage.Gallery)

    fun review(items: List<RecentMediaItem>) = MediaFlowLaunch(
      UUID.randomUUID().toString(),
      MediaStage.Review,
      uris = items.map { it.uri.toString() },
      videoUris = items.filter { it.isVideo }.map { it.uri.toString() },
    )

    /** For `rememberSaveable`: the launch as JSON. */
    val saver: Saver<MediaFlowLaunch?, String> = Saver(
      save = { maybe -> maybe?.let { Json.encodeToString(serializer(), it) } ?: "" },
      restore = { text -> text.takeIf { it.isNotEmpty() }?.let { runCatching { Json.decodeFromString(serializer(), it) }.getOrNull() } },
    )
  }
}

/**
 * The camera, gallery, review and editors as one full-screen flow (#1358). Its state is written to
 * a file as it changes; sending prepares every item on the device (edits applied, pictures
 * recompressed without metadata, videos trimmed and scaled) and hands the staged files and the
 * caption to the composer, which sends them like any attachment.
 */
@Composable
fun MediaFlow(
  viewModel: MainViewModel,
  key: MediaDraftKey,
  launch: MediaFlowLaunch,
  conversationName: String,
  initialCaption: String,
  onClose: () -> Unit,
  onSend: (List<PickedFile>, String) -> Unit,
) {
  val context = LocalContext.current
  val store = remember { MediaDraftStore(context) { viewModel.mediaCapturesRoot() } }
  val maybeCaptures by produceState<File?>(null, key) { value = store.capturesDirectory(key) }
  var maybeDraft by remember { mutableStateOf<MediaDraft?>(null) }
  var isSending by remember { mutableStateOf(false) }
  var isConfirmingDiscard by remember { mutableStateOf(false) }
  val scope = rememberCoroutineScope()
  LaunchedEffect(launch.id) {
    // The same opening after the app was killed comes back where it was; a new one starts fresh.
    maybeDraft = store.load(key)?.takeIf { it.launchId == launch.id } ?: run {
      store.clear(key)
      withContext(Dispatchers.IO) { fresh(context, launch, initialCaption) }
    }
  }
  val draft = maybeDraft
  LaunchedEffect(draft) {
    if (draft == null) return@LaunchedEffect
    delay(SAVE_DELAY_MS)
    store.save(key, draft)
  }
  fun update(new: MediaDraft) {
    maybeDraft = new
  }
  fun close() {
    scope.launch {
      store.clear(key)
      EditablePictures.clear()
      onClose()
    }
  }
  fun back() {
    val current = maybeDraft ?: return close()
    when {
      isSending -> Unit
      current.stage == MediaStage.Crop || current.stage == MediaStage.Draw -> update(current.copy(stage = MediaStage.Review))
      current.stage == MediaStage.Review -> update(current.copy(stage = current.origin))
      current.stage == MediaStage.Gallery && current.maybeAlbum != null -> update(current.copy(maybeAlbum = null))
      current.items.isNotEmpty() -> isConfirmingDiscard = true
      else -> close()
    }
  }
  fun send() {
    val current = maybeDraft ?: return
    isSending = true
    scope.launch {
      val maybeFiles = prepare(context, viewModel, current)
      isSending = false
      if (maybeFiles == null) {
        viewModel.showFailure(R.string.media_send_failed)
        return@launch
      }
      store.clear(key)
      EditablePictures.clear()
      onSend(maybeFiles, current.caption)
    }
  }
  Dialog(
    onDismissRequest = ::back,
    properties = DialogProperties(usePlatformDefaultWidth = false, decorFitsSystemWindows = false),
  ) {
    SystemBars(isDark = draft?.stage != MediaStage.Gallery || MaterialTheme.colorScheme.background.luminance() < 0.5f)
    Box(Modifier.fillMaxSize()) {
      if (draft == null) return@Box
      when (draft.stage) {
        // Shown once the account's staging folder is known: the camera writes there.
        MediaStage.Camera -> maybeCaptures?.let { captures ->
          CameraScreen(
            capturesDirectory = captures,
            selectedCount = draft.items.size,
            onCaptured = { item -> update(draft.added(listOf(item)).copy(stage = MediaStage.Review, origin = MediaStage.Camera)) },
            onGallery = { update(draft.copy(stage = MediaStage.Gallery)) },
            onReview = { update(draft.copy(stage = MediaStage.Review)) },
            onClose = ::back,
          )
        }
        MediaStage.Gallery -> GalleryScreen(
          draft = draft,
          onAlbum = { update(draft.copy(maybeAlbum = it)) },
          onToggle = { item -> update(toggled(draft, item)) },
          onPicked = { uris ->
            scope.launch {
              val picked = withContext(Dispatchers.IO) { uris.map { newItem(context, it) } }
              maybeDraft?.let { update(it.added(picked).copy(stage = MediaStage.Review, origin = MediaStage.Gallery)) }
            }
          },
          onNext = { update(draft.copy(stage = MediaStage.Review, origin = MediaStage.Gallery, currentIndex = draft.currentIndex.coerceIn(0, draft.items.size - 1))) },
          onCamera = { update(draft.copy(stage = MediaStage.Camera)) },
          onClose = ::back,
        )
        MediaStage.Review -> if (draft.items.isEmpty()) {
          LaunchedEffect(Unit) { update(draft.copy(stage = draft.origin)) }
        } else {
          ReviewScreen(
            draft = draft,
            conversationName = conversationName,
            isSending = isSending,
            onDraft = ::update,
            onCrop = { update(draft.copy(stage = MediaStage.Crop)) },
            onDraw = { update(draft.copy(stage = MediaStage.Draw)) },
            onSave = { draft.maybeCurrent?.let { item -> scope.launch { saveToDevice(context, viewModel, item, draft.quality) } } },
            onAddMore = { update(draft.copy(stage = MediaStage.Gallery)) },
            onBack = ::back,
            onSend = ::send,
          )
        }
        MediaStage.Crop -> draft.maybeCurrent?.let { item ->
          CropScreen(
            uri = item.uri,
            initial = item.picture,
            onCancel = ::back,
            onDone = { crop -> update(draft.updated(item.id) { it.copy(picture = it.picture.copy(crop = crop)) }.copy(stage = MediaStage.Review)) },
          )
        }
        MediaStage.Draw -> draft.maybeCurrent?.let { item ->
          DrawScreen(
            uri = item.uri,
            initial = item.picture,
            onCancel = ::back,
            onDone = { edit -> update(draft.updated(item.id) { it.copy(picture = edit) }.copy(stage = MediaStage.Review)) },
          )
        }
      }
    }
    if (isConfirmingDiscard) {
      val count = draft?.items?.size ?: 0
      AlertDialog(
        onDismissRequest = { isConfirmingDiscard = false },
        text = { Text(pluralStringResource(R.plurals.media_discard, count, count)) },
        confirmButton = {
          TextButton(onClick = {
            isConfirmingDiscard = false
            close()
          }) { Text(stringResource(R.string.media_discard_confirm)) }
        },
        dismissButton = { TextButton(onClick = { isConfirmingDiscard = false }) { Text(stringResource(R.string.cancel)) } },
      )
    }
  }
}

/** Light symbols on the dark camera, review and editors; the gallery follows the app's theme. */
@Composable
private fun SystemBars(isDark: Boolean) {
  val view = LocalView.current
  val maybeWindow = (view.parent as? DialogWindowProvider)?.window
  SideEffect {
    val window = maybeWindow ?: return@SideEffect
    WindowCompat.getInsetsController(window, view).apply {
      isAppearanceLightStatusBars = !isDark
      isAppearanceLightNavigationBars = !isDark
    }
  }
}

private fun fresh(context: Context, launch: MediaFlowLaunch, caption: String): MediaDraft {
  val quality = MediaPrefs.prefs(context).sendQuality
  val items = launch.uris.map { uri ->
    val isVideo = uri in launch.videoUris
    MediaDraftItem(id = UUID.randomUUID().toString(), uri = uri, isVideo = isVideo, durationMs = if (isVideo) durationOf(context, Uri.parse(uri)) else 0L)
  }
  return MediaDraft(launchId = launch.id, items = items, caption = caption, quality = quality, stage = launch.stage, origin = if (launch.stage == MediaStage.Review) MediaStage.Gallery else launch.stage)
}

private fun toggled(draft: MediaDraft, item: RecentMediaItem): MediaDraft {
  val uri = item.uri.toString()
  val existing = draft.items.firstOrNull { it.uri == uri }
  if (existing != null) return draft.removed(existing.id)
  if (draft.items.size >= MAX_MEDIA_ITEMS) return draft
  return draft.copy(items = draft.items + MediaDraftItem(id = UUID.randomUUID().toString(), uri = uri, isVideo = item.isVideo, durationMs = item.durationMs))
}

/** An item for a URI the system picker returned (it says nothing about the type, the resolver does). */
private fun newItem(context: Context, uri: Uri): MediaDraftItem {
  val isVideo = context.contentResolver.getType(uri)?.startsWith("video/") == true
  return MediaDraftItem(id = UUID.randomUUID().toString(), uri = uri.toString(), isVideo = isVideo, durationMs = if (isVideo) durationOf(context, uri) else 0L)
}

private fun durationOf(context: Context, uri: Uri): Long = runCatching {
  val retriever = android.media.MediaMetadataRetriever()
  try {
    retriever.setDataSource(context, uri)
    retriever.extractMetadata(android.media.MediaMetadataRetriever.METADATA_KEY_DURATION)?.toLongOrNull() ?: 0L
  } finally {
    retriever.release()
  }
}.getOrDefault(0L)

/**
 * Every item as a staged file, in order; `null` when one can't be prepared (what was staged goes
 * again, and the flow stays open with everything in it).
 */
private suspend fun prepare(context: Context, viewModel: MainViewModel, draft: MediaDraft): List<PickedFile>? {
  val staged = mutableListOf<PickedFile>()
  for (item in draft.items) {
    val maybeFile = runCatching { prepareItem(context, viewModel, item, draft.quality) }.getOrNull()
    if (maybeFile == null) {
      staged.forEach { viewModel.discardFile(it) }
      return null
    }
    staged += maybeFile
  }
  return staged
}

private suspend fun prepareItem(context: Context, viewModel: MainViewModel, item: MediaDraftItem, quality: MediaQuality): PickedFile? {
  val uri = Uri.parse(item.uri)
  val name = displayName(context, uri)
  if (!item.isVideo) {
    // Always drawn again: edits applied, the quality's size, no metadata of the original.
    val rendered = PictureRenderer.render(context, uri, item.picture, quality) ?: return null
    val ending = if (rendered.mediaType == "image/png") "png" else "jpg"
    return viewModel.stageEditedPicture("${name.substringBeforeLast('.', name)}.$ending", rendered.mediaType, rendered.bytes)
  }
  val facts = VideoEditing.facts(context, uri)
  val isEdited = VideoEditing.needsExport(facts, item.video, quality)
  // A video from the gallery is packed again even unedited (no re-encoding): that drops where it
  // was taken and the camera's details. The app's own camera records none.
  if (isEdited || !item.isCaptured) {
    // Sealed like the camera's files until it is staged, the one plain copy (e2ee.md §9.2).
    val target = sealedExportFile(context, SealedCaptures.EXPORT_PREFIX)
    try {
      if (VideoEditing.export(context, uri, item.video, facts, quality, target)) {
        return viewModel.stageSealedFile("${name.substringBeforeLast('.', name)}.mp4", "video/mp4", Uri.fromFile(target))
      }
    } finally {
      withContext(NonCancellable + Dispatchers.IO) { target.delete() }
    }
    // An edit the device can't do fails the send; an unedited video it can't repack goes as it
    // is, as before (docs/e2ee.md §9.2 says so).
    if (isEdited) return null
  }
  return when {
    // The sealed original stays until the flow ends: a send that fails can go again.
    SealedCaptures.isSealed(uri) -> viewModel.stageSealedFile(name, "video/mp4", uri)
    item.isCaptured -> uri.path?.let { viewModel.stageMovedFile(name, "video/mp4", File(it)) }
    else -> viewModel.importFile(uri)
  }
}

/** Where Transformer's sealed output waits in the cache; a kill's leftovers go at the next start. */
private fun sealedExportFile(context: Context, prefix: String): File =
  File(context.cacheDir, "$prefix${System.nanoTime()}.mp4${SealedCaptures.SUFFIX}")

private suspend fun displayName(context: Context, uri: Uri): String = withContext(Dispatchers.IO) {
  if (uri.scheme == "file") return@withContext uri.lastPathSegment?.let(SealedCaptures::plainName) ?: "media"
  runCatching {
    context.contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { cursor ->
      if (cursor.moveToFirst()) cursor.getString(0) else null
    }
  }.getOrNull()?.takeIf { it.isNotBlank() } ?: "media"
}

/**
 * Saves the item as it will be sent (edits applied) into the device's gallery, under Pictures or
 * Movies / TeamCraft. Android 10 and later only: no storage permission is needed there.
 */
private suspend fun saveToDevice(context: Context, viewModel: MainViewModel, item: MediaDraftItem, quality: MediaQuality) {
  if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) return
  val isSaved = runCatching {
    val uri = Uri.parse(item.uri)
    if (item.isVideo) {
      val facts = VideoEditing.facts(context, uri)
      val isSealed = SealedCaptures.isSealed(uri)
      val maybeExport = if (VideoEditing.needsExport(facts, item.video, quality)) {
        sealedExportFile(context, SealedCaptures.SAVE_PREFIX).takeIf { VideoEditing.export(context, uri, item.video, facts, quality, it) }
      } else {
        null
      }
      withContext(Dispatchers.IO) {
        try {
          insert(context, MediaStore.Video.Media.EXTERNAL_CONTENT_URI, Environment.DIRECTORY_MOVIES, "video-${System.currentTimeMillis()}.mp4", "video/mp4") { out ->
            val maybeInput = when {
              maybeExport != null -> SealedCaptures.inputStream(context, Uri.fromFile(maybeExport))
              isSealed -> SealedCaptures.inputStream(context, uri)
              else -> context.contentResolver.openInputStream(uri)
            }
            maybeInput?.use { it.copyTo(out) } != null
          }
        } finally {
          maybeExport?.delete()
        }
      }
    } else {
      val rendered = PictureRenderer.render(context, uri, item.picture, MediaQuality.High) ?: return@runCatching false
      val ending = if (rendered.mediaType == "image/png") "png" else "jpg"
      withContext(Dispatchers.IO) {
        insert(context, MediaStore.Images.Media.EXTERNAL_CONTENT_URI, Environment.DIRECTORY_PICTURES, "picture-${System.currentTimeMillis()}.$ending", rendered.mediaType) { out ->
          out.write(rendered.bytes)
          true
        }
      }
    }
  }.getOrDefault(false)
  if (isSaved) Toast.makeText(context, R.string.media_saved, Toast.LENGTH_SHORT).show() else viewModel.showFailure(R.string.media_save_failed)
}

private fun insert(context: Context, collection: Uri, directory: String, name: String, mediaType: String, write: (java.io.OutputStream) -> Boolean): Boolean {
  val resolver = context.contentResolver
  val values = ContentValues().apply {
    put(MediaStore.MediaColumns.DISPLAY_NAME, name)
    put(MediaStore.MediaColumns.MIME_TYPE, mediaType)
    put(MediaStore.MediaColumns.RELATIVE_PATH, "$directory/TeamCraft")
    put(MediaStore.MediaColumns.IS_PENDING, 1)
  }
  val target = resolver.insert(collection, values) ?: return false
  val isWritten = runCatching { resolver.openOutputStream(target)?.use(write) ?: false }.getOrDefault(false)
  if (!isWritten) {
    resolver.delete(target, null, null)
    return false
  }
  resolver.update(target, ContentValues().apply { put(MediaStore.MediaColumns.IS_PENDING, 0) }, null, null)
  return true
}
