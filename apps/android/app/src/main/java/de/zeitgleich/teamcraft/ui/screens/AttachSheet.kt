package de.zeitgleich.teamcraft.ui.screens

import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.annotation.DrawableRes
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.layout.width
import androidx.compose.ui.draw.clip
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.launch
import android.text.format.DateUtils
import androidx.compose.foundation.Image
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.Button
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.produceState
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import de.zeitgleich.teamcraft.ui.MediaAccess
import de.zeitgleich.teamcraft.ui.RecentMediaItem
import de.zeitgleich.teamcraft.ui.recentMedia
import de.zeitgleich.teamcraft.ui.recentMediaThumbnail
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.PickedFile
import de.zeitgleich.teamcraft.ui.MainViewModel
import de.zeitgleich.teamcraft.ui.media.MediaFlowLaunch

/** The composer's "+": the camera, the gallery (the app's own, #1358), or any file. */
class AttachmentPickers(
  val openCamera: () -> Unit,
  val openGallery: () -> Unit,
  val pickFiles: () -> Unit,
  /** Opens the review with photos and videos chosen in the attach sheet's strip of recent ones. */
  val reviewMedia: (List<RecentMediaItem>) -> Unit,
)

/**
 * Pickers for the composer: files go through the system's picker and are imported into the core's
 * staging folder and handed to `onPicked`; camera, gallery and the recent strip open the app's
 * media flow ([de.zeitgleich.teamcraft.ui.media.MediaFlow]).
 */
@Composable
fun rememberAttachmentPickers(
  viewModel: MainViewModel,
  onMediaFlow: (MediaFlowLaunch) -> Unit,
  onPicked: (List<PickedFile>) -> Unit,
): AttachmentPickers {
  val scope = rememberCoroutineScope()
  val files = rememberLauncherForActivityResult(ActivityResultContracts.GetMultipleContents()) { uris ->
    scope.launch { onPicked(uris.mapNotNull { viewModel.importFile(it) }) }
  }
  return remember(files, onMediaFlow) {
    AttachmentPickers(
      openCamera = { onMediaFlow(MediaFlowLaunch.camera()) },
      openGallery = { onMediaFlow(MediaFlowLaunch.gallery()) },
      pickFiles = { files.launch("*/*") },
      reviewMedia = { onMediaFlow(MediaFlowLaunch.review(it)) },
    )
  }
}

/**
 * What can be attached: the newest photos and videos of the device as a strip to pick from (as in
 * the common messengers), then camera, gallery and file as one row of tiles in thumb reach. How a
 * message is sent (priority, receipts, silent) is not here: that is the send button's long-press.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AttachSheet(pickers: AttachmentPickers, onDismiss: () -> Unit) {
  ModalBottomSheet(onDismissRequest = onDismiss) {
    // Not in the Play build: there the gallery tile opens the system photo picker.
    if (MediaAccess.isOffered) {
      RecentMediaStrip(
        onAdd = { items ->
          onDismiss()
          pickers.reviewMedia(items)
        },
      )
    }
    Row(
      Modifier.fillMaxWidth().padding(start = 16.dp, end = 16.dp, top = 4.dp, bottom = 28.dp),
      horizontalArrangement = Arrangement.SpaceEvenly,
    ) {
      AttachOption(R.drawable.ic_photo_camera, R.string.attach_camera) {
        onDismiss()
        pickers.openCamera()
      }
      AttachOption(R.drawable.ic_image, R.string.attach_gallery) {
        onDismiss()
        pickers.openGallery()
      }
      AttachOption(R.drawable.ic_description, R.string.attach_file) {
        onDismiss()
        pickers.pickFiles()
      }
    }
  }
}

/**
 * The newest photos and videos, picked by a tap (several at once) and added with the button that
 * shows then. Without access, one tile asks for it; with Android 14's "selected photos" a tile lets
 * the user pick more. The gallery tile below always works without any access.
 */
@Composable
private fun RecentMediaStrip(onAdd: (List<RecentMediaItem>) -> Unit) {
  val context = LocalContext.current
  var access by remember { mutableStateOf(MediaAccess.of(context)) }
  var items by remember { mutableStateOf<List<RecentMediaItem>>(emptyList()) }
  var selected by remember { mutableStateOf(listOf<RecentMediaItem>()) }
  val ask = rememberLauncherForActivityResult(ActivityResultContracts.RequestMultiplePermissions()) {
    access = MediaAccess.of(context)
  }
  LaunchedEffect(access) {
    items = if (access == MediaAccess.None) emptyList() else recentMedia(context)
  }
  LazyRow(
    Modifier.fillMaxWidth().padding(bottom = 8.dp),
    contentPadding = PaddingValues(horizontal = 16.dp),
    horizontalArrangement = Arrangement.spacedBy(6.dp),
  ) {
    if (access != MediaAccess.Full) {
      item(key = "access") {
        AccessTile(
          label = stringResource(if (access == MediaAccess.None) R.string.attach_recent_allow else R.string.attach_recent_more),
          onClick = { ask.launch(MediaAccess.permissions()) },
        )
      }
    }
    items(items, key = { it.uri.toString() }) { item ->
      val index = selected.indexOf(item)
      MediaTile(item, selectionNumber = index + 1) {
        selected = if (index >= 0) selected - item else selected + item
      }
    }
  }
  if (selected.isNotEmpty()) {
    Button(
      onClick = { onAdd(selected) },
      modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp),
    ) { Text(pluralStringResource(R.plurals.attach_recent_add, selected.size, selected.size)) }
  }
}

@Composable
private fun AccessTile(label: String, onClick: () -> Unit) {
  Column(
    Modifier
      .size(MEDIA_TILE)
      .clip(RoundedCornerShape(12.dp))
      .background(MaterialTheme.colorScheme.secondaryContainer)
      .clickable(onClick = onClick)
      .padding(8.dp),
    horizontalAlignment = Alignment.CenterHorizontally,
    verticalArrangement = Arrangement.Center,
  ) {
    Icon(painterResource(R.drawable.ic_image), contentDescription = null, tint = MaterialTheme.colorScheme.onSecondaryContainer)
    Text(label, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSecondaryContainer, textAlign = TextAlign.Center, maxLines = 3)
  }
}

/** One photo or video: its thumbnail, a video's length, and its number in the selection. */
@Composable
private fun MediaTile(item: RecentMediaItem, selectionNumber: Int, onToggle: () -> Unit) {
  val context = LocalContext.current
  val sidePx = with(LocalDensity.current) { MEDIA_TILE.roundToPx() }
  val maybeThumbnail by produceState<ImageBitmap?>(null, item.uri) {
    value = recentMediaThumbnail(context, item, sidePx)?.asImageBitmap()
  }
  val isSelected = selectionNumber > 0
  val description = stringResource(if (item.isVideo) R.string.attach_recent_video else R.string.attach_recent_photo)
  Box(
    Modifier
      .size(MEDIA_TILE)
      .clip(RoundedCornerShape(12.dp))
      .background(MaterialTheme.colorScheme.surfaceContainerHighest)
      .toggleable(value = isSelected, onValueChange = { onToggle() })
      .semantics { contentDescription = description },
  ) {
    maybeThumbnail?.let { Image(it, contentDescription = null, contentScale = ContentScale.Crop, modifier = Modifier.matchParentSize()) }
    if (isSelected) Box(Modifier.matchParentSize().background(Color.Black.copy(alpha = 0.25f)))
    if (item.isVideo) {
      Text(
        DateUtils.formatElapsedTime(item.durationMs / 1000),
        style = MaterialTheme.typography.labelSmall,
        color = Color.White,
        modifier = Modifier.align(Alignment.BottomStart).padding(4.dp).background(Color.Black.copy(alpha = 0.5f), RoundedCornerShape(6.dp)).padding(horizontal = 4.dp),
      )
    }
    Box(
      Modifier
        .align(Alignment.TopEnd)
        .padding(5.dp)
        .size(22.dp)
        .clip(CircleShape)
        .background(if (isSelected) MaterialTheme.colorScheme.primary else Color.Black.copy(alpha = 0.3f))
        .border(1.5.dp, Color.White, CircleShape),
      contentAlignment = Alignment.Center,
    ) {
      if (isSelected) Text(selectionNumber.toString(), style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onPrimary)
    }
  }
}

private val MEDIA_TILE = 96.dp

/** One picker: the symbol on a tinted rounded square with its label below (the whole tile is the target). */
@Composable
private fun AttachOption(@DrawableRes icon: Int, label: Int, onClick: () -> Unit) {
  Column(
    Modifier.width(96.dp).clip(RoundedCornerShape(16.dp)).clickable(onClick = onClick).padding(vertical = 8.dp),
    horizontalAlignment = Alignment.CenterHorizontally,
    verticalArrangement = Arrangement.spacedBy(8.dp),
  ) {
    Box(
      Modifier.size(56.dp).background(MaterialTheme.colorScheme.secondaryContainer, RoundedCornerShape(16.dp)),
      contentAlignment = Alignment.Center,
    ) {
      // Decorative: the label under it names the action.
      Icon(painterResource(icon), contentDescription = null, tint = MaterialTheme.colorScheme.onSecondaryContainer)
    }
    Text(
      stringResource(label),
      style = MaterialTheme.typography.labelLarge,
      textAlign = TextAlign.Center,
      maxLines = 2,
    )
  }
}
