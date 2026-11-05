package de.zeitgleich.teamcraft.ui.screens

import android.text.format.Formatter
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Icon
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import java.io.File
import kotlinx.coroutines.delay
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.ChatActions
import de.zeitgleich.teamcraft.core.ChatQueries
import de.zeitgleich.teamcraft.core.ChatSnapshot
import de.zeitgleich.teamcraft.core.FileInfo
import de.zeitgleich.teamcraft.ui.MainViewModel
import de.zeitgleich.teamcraft.ui.PictureTarget
import de.zeitgleich.teamcraft.ui.THUMBNAIL_DECODE_PX
import de.zeitgleich.teamcraft.ui.rememberThumbnail
import de.zeitgleich.teamcraft.ui.thumbnailKey

/** Every file of a conversation as a gallery: images as thumbnails, tap to download or open. */
@Composable
fun FilesScreen(viewModel: MainViewModel, conversationId: String, onBack: () -> Unit) {
  val files = rememberFiles(viewModel, ChatQueries.files(conversationId))
  Scaffold(topBar = { ChromeTopBar(title = stringResource(R.string.files_title), maybeOnBack = onBack) }, contentWindowInsets = ScreenInsets) { padding ->
    if (files?.isEmpty() == true) {
      EmptyText(R.string.files_empty)
    }
    FilesGrid(viewModel, files.orEmpty(), Modifier.fillMaxSize().padding(padding))
  }
}

/** The answer of a files query, kept fresh; `null` until the first one. */
@Composable
internal fun rememberFiles(viewModel: MainViewModel, request: String): List<FileInfo>? {
  val changes by viewModel.changeCount.collectAsState()
  var files by remember(request) { mutableStateOf<List<FileInfo>?>(null) }
  LaunchedEffect(changes, request) {
    files = viewModel.readChat { query(request).files }
  }
  LaunchedEffect(files) {
    // Downloads report their end in the store, not every chunk: follow them while they run.
    if (files.orEmpty().any { it.transfer.isMoving }) {
      delay(1000)
      files = viewModel.readChat { query(request).files } ?: files
    }
  }
  return files
}

/** The tiles of [files]: a picture opens in the viewer, anything else from disk or after its download. */
@Composable
internal fun FilesGrid(viewModel: MainViewModel, files: List<FileInfo>, modifier: Modifier) {
  val context = LocalContext.current
  // Saveable: a rotation keeps the viewer open (the conversation, message and file it opened on).
  var maybeViewing by rememberSaveable { mutableStateOf<List<String>?>(null) }
  var maybeVideo by rememberSaveable { mutableStateOf<List<String>?>(null) }
  // Names for the viewer's sender line and its forward targets; read when it opens.
  var maybeSnapshot by remember { mutableStateOf<ChatSnapshot?>(null) }
  LaunchedEffect(maybeViewing != null) {
    if (maybeViewing != null && maybeSnapshot == null) maybeSnapshot = viewModel.readChat { snapshot() }
  }
  LazyVerticalGrid(GridCells.Adaptive(120.dp), modifier) {
    items(files, key = { it.fileId }) { file ->
      Column(
        Modifier.padding(4.dp).clickable {
          val path = file.transfer.path
          if (file.isImage && file.maybeThumbnail != null) {
            // The viewer shows the thumbnail at once and fetches the original itself.
            maybeViewing = PictureTarget.of(file).toList()
          } else if (file.opensInVideoPlayer) {
            maybeVideo = PictureTarget.of(file).toList()
          } else if (file.transfer.state == "local" && path != null) {
            openFile(context, File(path), file.mediaType)
          } else {
            viewModel.act(ChatActions.download(file.conversation, file.message))
          }
        },
      ) {
        val maybeThumbnail by rememberThumbnail(thumbnailKey(file), file.maybeThumbnail?.bytes, THUMBNAIL_DECODE_PX)
        Box(
          Modifier.fillMaxWidth().aspectRatio(1f).clip(RoundedCornerShape(8.dp)).background(MaterialTheme.colorScheme.surfaceVariant),
          contentAlignment = Alignment.Center,
        ) {
          val thumbnail = maybeThumbnail
          if (thumbnail != null) {
            Image(thumbnail, contentDescription = file.fileName, contentScale = ContentScale.Crop, modifier = Modifier.fillMaxSize())
          } else if (file.maybeThumbnail == null) {
            Icon(
              painterResource(if (file.isImage) R.drawable.ic_image else R.drawable.ic_description),
              contentDescription = null,
              tint = MaterialTheme.colorScheme.onSurfaceVariant,
              modifier = Modifier.size(40.dp),
            )
          }
          if (file.isVideo) PlayMark()
        }
        Text(file.fileName, maxLines = 1, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.bodySmall)
        Text(Formatter.formatShortFileSize(context, file.sizeBytes), style = MaterialTheme.typography.labelSmall)
        file.transfer.maybeProgress?.let { LinearProgressIndicator(progress = { it }, modifier = Modifier.fillMaxWidth()) }
      }
    }
  }
  maybeVideo?.let(PictureTarget::fromList)?.let { target ->
    VideoViewer(viewModel, target, onDismiss = { maybeVideo = null })
  }
  maybeViewing?.let(PictureTarget::fromList)?.let { target ->
    ImageViewer(viewModel, maybeSnapshot, target, onDismiss = { maybeViewing = null })
  }
}
