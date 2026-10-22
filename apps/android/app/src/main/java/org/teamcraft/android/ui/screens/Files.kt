package org.teamcraft.android.ui.screens

import android.graphics.BitmapFactory
import android.text.format.Formatter
import androidx.compose.foundation.Image
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.items
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
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import java.io.File
import org.teamcraft.android.R
import org.teamcraft.android.core.ChatActions
import org.teamcraft.android.core.ChatQueries
import org.teamcraft.android.core.FileInfo
import org.teamcraft.android.ui.MainViewModel

/** Every file of a conversation as a gallery: images as thumbnails, tap to download or open. */
@Composable
fun FilesScreen(viewModel: MainViewModel, conversationId: String, onBack: () -> Unit) {
  val changes by viewModel.changeCount.collectAsState()
  val context = LocalContext.current
  var files by remember { mutableStateOf<List<FileInfo>?>(null) }
  LaunchedEffect(changes, conversationId) {
    files = viewModel.readChat { query(ChatQueries.files(conversationId)).files }
  }
  Scaffold(topBar = { ChromeTopBar(title = stringResource(R.string.files_title), maybeOnBack = onBack) }) { padding ->
    if (files?.isEmpty() == true) {
      EmptyText(R.string.files_empty)
    }
    LazyVerticalGrid(GridCells.Adaptive(120.dp), Modifier.fillMaxSize().padding(padding)) {
      items(files.orEmpty(), key = { it.fileId }) { file ->
        Column(
          Modifier.padding(4.dp).clickable {
            val path = file.transfer.path
            if (file.transfer.state == "local" && path != null) {
              openFile(context, File(path), file.mediaType)
            } else {
              viewModel.act(ChatActions.download(file.conversation, file.message))
            }
          },
        ) {
          val bitmap = file.thumbnailBytes()?.let { BitmapFactory.decodeByteArray(it, 0, it.size) }
          if (bitmap != null) {
            Image(
              bitmap.asImageBitmap(),
              contentDescription = file.fileName,
              contentScale = ContentScale.Crop,
              modifier = Modifier.fillMaxWidth().aspectRatio(1f),
            )
          } else {
            Text("📄", style = MaterialTheme.typography.displayMedium, modifier = Modifier.padding(16.dp))
          }
          Text(file.fileName, maxLines = 1, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.bodySmall)
          Text(Formatter.formatShortFileSize(context, file.sizeBytes), style = MaterialTheme.typography.labelSmall)
          file.transfer.maybeProgress?.let { LinearProgressIndicator(progress = { it }, modifier = Modifier.fillMaxWidth()) }
        }
      }
    }
  }
}
