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
  val changes by viewModel.changeCount.collectAsState()
  val context = LocalContext.current
  var files by remember { mutableStateOf<List<FileInfo>?>(null) }
  // Saveable: a rotation keeps the viewer open (the conversation, message and file it opened on).
  var maybeViewing by rememberSaveable { mutableStateOf<List<String>?>(null) }
  // Names for the viewer's sender line and its forward targets; read when it opens.
  var maybeSnapshot by remember { mutableStateOf<ChatSnapshot?>(null) }
  LaunchedEffect(maybeViewing != null) {
    if (maybeViewing != null && maybeSnapshot == null) maybeSnapshot = viewModel.readChat { snapshot() }
  }
  LaunchedEffect(changes, conversationId) {
    files = viewModel.readChat { query(ChatQueries.files(conversationId)).files }
  }
  LaunchedEffect(files) {
    // Downloads report their end in the store, not every chunk: follow them while they run.
    if (files.orEmpty().any { it.transfer.isMoving }) {
      delay(1000)
      files = viewModel.readChat { query(ChatQueries.files(conversationId)).files } ?: files
    }
  }
  Scaffold(topBar = { ChromeTopBar(title = stringResource(R.string.files_title), maybeOnBack = onBack) }, contentWindowInsets = ScreenInsets) { padding ->
    if (files?.isEmpty() == true) {
      EmptyText(R.string.files_empty)
    }
    LazyVerticalGrid(GridCells.Adaptive(120.dp), Modifier.fillMaxSize().padding(padding)) {
      items(files.orEmpty(), key = { it.fileId }) { file ->
        Column(
          Modifier.padding(4.dp).clickable {
            val path = file.transfer.path
            if (file.isImage && file.maybeThumbnail != null) {
              // The viewer shows the thumbnail at once and fetches the original itself.
              maybeViewing = PictureTarget.of(file).toList()
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
          }
          Text(file.fileName, maxLines = 1, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.bodySmall)
          Text(Formatter.formatShortFileSize(context, file.sizeBytes), style = MaterialTheme.typography.labelSmall)
          file.transfer.maybeProgress?.let { LinearProgressIndicator(progress = { it }, modifier = Modifier.fillMaxWidth()) }
        }
      }
    }
  }
  maybeViewing?.let(PictureTarget::fromList)?.let { target ->
    ImageViewer(viewModel, maybeSnapshot, target, onDismiss = { maybeViewing = null })
  }
}
