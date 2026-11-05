package de.zeitgleich.teamcraft.ui.screens

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.net.Uri
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
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
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import androidx.core.content.FileProvider
import java.io.File
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

/** Where camera photos wait (app cache, shared with the camera app through the FileProvider). */
private const val CAMERA_DIR = "camera"

/** The composer's "+": take a photo, pick photos and videos, or any file. */
class AttachmentPickers(
  val takePhoto: () -> Unit,
  val pickMedia: () -> Unit,
  val pickFiles: () -> Unit,
  /** Imports photos and videos chosen in the attach sheet's strip of recent ones. */
  val addMedia: (List<Uri>) -> Unit,
)

/**
 * Pickers that import what the user chose into the core's staging folder and hand it to
 * `onPicked`. The photo picker needs no storage permission; the camera asks for its permission
 * first, because the app declares it (calls) and Android then requires it for capture intents too.
 */
@Composable
fun rememberAttachmentPickers(viewModel: MainViewModel, onPicked: (List<PickedFile>) -> Unit): AttachmentPickers {
  val context = LocalContext.current
  val scope = rememberCoroutineScope()
  var maybePhotoPath by rememberSaveable { mutableStateOf<String?>(null) }
  fun import(uris: List<Uri>) {
    scope.launch { onPicked(uris.mapNotNull { viewModel.importFile(it) }) }
  }
  val media = rememberLauncherForActivityResult(ActivityResultContracts.PickMultipleVisualMedia()) { import(it) }
  val files = rememberLauncherForActivityResult(ActivityResultContracts.GetMultipleContents()) { import(it) }
  val camera = rememberLauncherForActivityResult(ActivityResultContracts.TakePicture()) { isTaken ->
    val path = maybePhotoPath ?: return@rememberLauncherForActivityResult
    maybePhotoPath = null
    val photo = File(path)
    if (!isTaken) {
      photo.delete()
      return@rememberLauncherForActivityResult
    }
    scope.launch {
      onPicked(listOfNotNull(viewModel.importPhoto(photo)))
      // The staged copy is what gets sent; the camera's original is no longer needed.
      photo.delete()
    }
  }
  fun launchCamera() {
    val photo = newPhotoFile(context)
    maybePhotoPath = photo.absolutePath
    camera.launch(FileProvider.getUriForFile(context, "${context.packageName}.files", photo))
  }
  val permission = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { isGranted ->
    if (isGranted) launchCamera()
  }
  return remember(media, files, camera, permission) {
    AttachmentPickers(
      takePhoto = {
        if (ContextCompat.checkSelfPermission(context, Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED) {
          launchCamera()
        } else {
          permission.launch(Manifest.permission.CAMERA)
        }
      },
      pickMedia = { media.launch(PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageAndVideo)) },
      pickFiles = { files.launch("*/*") },
      addMedia = { import(it) },
    )
  }
}

private fun newPhotoFile(context: Context): File {
  val directory = File(context.cacheDir, CAMERA_DIR).apply { mkdirs() }
  return File(directory, "photo-${System.currentTimeMillis()}.jpg")
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
    RecentMediaStrip(
      onAdd = { uris ->
        onDismiss()
        pickers.addMedia(uris)
      },
    )
    Row(
      Modifier.fillMaxWidth().padding(start = 16.dp, end = 16.dp, top = 4.dp, bottom = 28.dp),
      horizontalArrangement = Arrangement.SpaceEvenly,
    ) {
      AttachOption(R.drawable.ic_photo_camera, R.string.attach_camera) {
        onDismiss()
        pickers.takePhoto()
      }
      AttachOption(R.drawable.ic_image, R.string.attach_gallery) {
        onDismiss()
        pickers.pickMedia()
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
private fun RecentMediaStrip(onAdd: (List<Uri>) -> Unit) {
  val context = LocalContext.current
  var access by remember { mutableStateOf(MediaAccess.of(context)) }
  var items by remember { mutableStateOf<List<RecentMediaItem>>(emptyList()) }
  var selected by remember { mutableStateOf(listOf<Uri>()) }
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
      val index = selected.indexOf(item.uri)
      MediaTile(item, selectionNumber = index + 1) {
        selected = if (index >= 0) selected - item.uri else selected + item.uri
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
