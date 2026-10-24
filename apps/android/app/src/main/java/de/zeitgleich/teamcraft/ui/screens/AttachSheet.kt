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
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
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
    )
  }
}

private fun newPhotoFile(context: Context): File {
  val directory = File(context.cacheDir, CAMERA_DIR).apply { mkdirs() }
  return File(directory, "photo-${System.currentTimeMillis()}.jpg")
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AttachSheet(pickers: AttachmentPickers, onDismiss: () -> Unit, extra: @Composable ColumnScope.() -> Unit = {}) {
  ModalBottomSheet(onDismissRequest = onDismiss) {
    Column(Modifier.padding(bottom = 24.dp)) {
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
      extra()
    }
  }
}

/** One picker: the symbol in a tinted circle, then its label (the whole row is the target). */
@Composable
private fun AttachOption(@DrawableRes icon: Int, label: Int, onClick: () -> Unit) {
  Row(
    Modifier.fillMaxWidth().clickable(onClick = onClick).padding(horizontal = 24.dp, vertical = 10.dp),
    verticalAlignment = Alignment.CenterVertically,
  ) {
    Box(
      Modifier.size(40.dp).background(MaterialTheme.colorScheme.secondaryContainer, CircleShape),
      contentAlignment = Alignment.Center,
    ) {
      // Decorative: the label next to it names the action.
      Icon(painterResource(icon), contentDescription = null, tint = MaterialTheme.colorScheme.onSecondaryContainer)
    }
    Text(stringResource(label), modifier = Modifier.padding(start = 16.dp), style = MaterialTheme.typography.bodyLarge)
  }
}
