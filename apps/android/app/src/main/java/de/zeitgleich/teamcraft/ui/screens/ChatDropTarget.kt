package de.zeitgleich.teamcraft.ui.screens

import android.app.Activity
import android.content.Context
import android.net.Uri
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.draganddrop.dragAndDropTarget
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.compositionLocalOf
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draganddrop.DragAndDropEvent
import androidx.compose.ui.draganddrop.DragAndDropTarget
import androidx.compose.ui.draganddrop.mimeTypes
import androidx.compose.ui.draganddrop.toAndroidDragEvent
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.ui.ChatDrop
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch

/** The conversation's drop state, set by [ChatScaffold]; null outside a conversation. */
internal val LocalChatDrop = compositionLocalOf<ChatDrop?> { null }

/**
 * Makes the screen take files dragged onto it (#2074). Plain text drags are left to the text field.
 */
@OptIn(ExperimentalFoundationApi::class)
internal fun Modifier.chatDropTarget(drop: ChatDrop): Modifier = dragAndDropTarget(
  shouldStartDragAndDrop = { event -> drop.maybeHandler != null && event.mimeTypes().any { it != "text/plain" } },
  target = object : DragAndDropTarget {
    override fun onEntered(event: DragAndDropEvent) {
      drop.isOver = true
    }

    override fun onExited(event: DragAndDropEvent) {
      drop.isOver = false
    }

    override fun onEnded(event: DragAndDropEvent) {
      drop.isOver = false
    }

    override fun onDrop(event: DragAndDropEvent): Boolean {
      drop.isOver = false
      return drop.maybeHandler?.invoke(event) == true
    }
  },
)

/** The frame and label shown while files are over the conversation. */
@Composable
internal fun DropHighlight() {
  Box(
    Modifier
      .fillMaxSize()
      .padding(8.dp)
      .background(MaterialTheme.colorScheme.primary.copy(alpha = 0.12f), RoundedCornerShape(16.dp))
      .border(2.dp, MaterialTheme.colorScheme.primary, RoundedCornerShape(16.dp)),
    contentAlignment = Alignment.Center,
  ) {
    Text(stringResource(R.string.drop_files_here), style = MaterialTheme.typography.titleMedium, color = MaterialTheme.colorScheme.primary)
  }
}

/**
 * Takes the files of a drop: the read permission for the dropped URIs is asked of the activity and
 * kept until each is copied into the staging folder by [import]. Returns whether anything was taken.
 */
internal fun acceptDroppedFiles(context: Context, event: DragAndDropEvent, scope: CoroutineScope, import: suspend (Uri) -> Unit): Boolean {
  val android = event.toAndroidDragEvent()
  val clip = android.clipData ?: return false
  val uris = (0 until clip.itemCount).mapNotNull { clip.getItemAt(it).uri }
  if (uris.isEmpty()) return false
  val permissions = context.findActivity()?.requestDragAndDropPermissions(android)
  scope.launch {
    try {
      uris.forEach { import(it) }
    } finally {
      permissions?.release()
    }
  }
  return true
}
