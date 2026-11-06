package de.zeitgleich.teamcraft.ui.media

import android.content.Context
import android.net.Uri
import android.text.format.DateUtils
import android.util.LruCache
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.GridItemSpan
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.items
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.ui.MediaAccess
import de.zeitgleich.teamcraft.ui.RecentMediaItem
import de.zeitgleich.teamcraft.ui.recentMediaThumbnail

/** How many photos and videos one message takes at most from the gallery. */
const val MAX_MEDIA_ITEMS = 32

/**
 * The gallery as in the common messengers (#1358): albums as a grid (Camera, all media, the
 * device's folders) with the camera top right; an album is a dense grid where a tap selects with a
 * number, videos show their length, and the selection is a strip at the bottom with its count and
 * a next button. In the app's theme (light or dark).
 */
@Composable
fun GalleryScreen(
  draft: MediaDraft,
  onAlbum: (MediaAlbumRef?) -> Unit,
  onToggle: (RecentMediaItem) -> Unit,
  onPicked: (List<Uri>) -> Unit,
  onNext: () -> Unit,
  onCamera: () -> Unit,
  onClose: () -> Unit,
) {
  val context = LocalContext.current
  var access by remember { mutableStateOf(MediaAccess.of(context)) }
  val ask = rememberLauncherForActivityResult(ActivityResultContracts.RequestMultiplePermissions()) {
    access = MediaAccess.of(context)
  }
  // The system's picker works without any access; what it returns is added like a selection.
  val picker = rememberLauncherForActivityResult(ActivityResultContracts.PickMultipleVisualMedia(MAX_MEDIA_ITEMS)) { uris ->
    if (uris.isNotEmpty()) onPicked(uris)
  }
  fun openPicker() = picker.launch(PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageAndVideo))
  val maybeAlbum = draft.maybeAlbum
  Surface(Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.background) {
    Column(Modifier.fillMaxSize().windowInsetsPadding(WindowInsets.safeDrawing)) {
      Row(Modifier.fillMaxWidth().padding(horizontal = 4.dp, vertical = 4.dp), verticalAlignment = Alignment.CenterVertically) {
        IconButton(onClick = { if (maybeAlbum != null) onAlbum(null) else onClose() }) {
          Icon(
            painterResource(if (maybeAlbum != null) R.drawable.ic_arrow_back else R.drawable.ic_close),
            contentDescription = stringResource(if (maybeAlbum != null) R.string.media_back else R.string.media_close),
          )
        }
        Text(
          maybeAlbum?.let { albumName(it) } ?: stringResource(R.string.media_gallery),
          style = MaterialTheme.typography.titleLarge,
          maxLines = 1,
          overflow = TextOverflow.Ellipsis,
          modifier = Modifier.weight(1f).padding(start = 4.dp),
        )
        IconButton(onClick = onCamera) {
          Icon(painterResource(R.drawable.ic_photo_camera), contentDescription = stringResource(R.string.attach_camera))
        }
      }
      Box(Modifier.weight(1f)) {
        when {
          access == MediaAccess.None -> NoAccess(onAllow = { ask.launch(MediaAccess.permissions()) }, onPicker = ::openPicker)
          maybeAlbum == null -> Albums(
            isPartial = access == MediaAccess.Partial,
            onMore = { ask.launch(MediaAccess.permissions()) },
            onAlbum = onAlbum,
          )
          else -> AlbumGrid(maybeAlbum, draft, isFull = draft.items.size >= MAX_MEDIA_ITEMS, onToggle = onToggle)
        }
      }
      if (draft.items.isNotEmpty()) SelectionBar(draft, onNext)
    }
  }
}

@Composable
private fun albumName(album: MediaAlbumRef): String = when (album.kind) {
  MediaAlbumKind.Camera -> stringResource(R.string.media_album_camera)
  MediaAlbumKind.All -> stringResource(R.string.media_album_all)
  MediaAlbumKind.Folder -> album.name.ifBlank { stringResource(R.string.media_album_untitled) }
}

@Composable
private fun NoAccess(onAllow: () -> Unit, onPicker: () -> Unit) {
  Column(
    Modifier.fillMaxSize().padding(32.dp),
    horizontalAlignment = Alignment.CenterHorizontally,
    verticalArrangement = Arrangement.spacedBy(16.dp, Alignment.CenterVertically),
  ) {
    Icon(painterResource(R.drawable.ic_photo_library), contentDescription = null, modifier = Modifier.size(48.dp), tint = MaterialTheme.colorScheme.onSurfaceVariant)
    Text(stringResource(R.string.media_gallery_access), textAlign = TextAlign.Center, style = MaterialTheme.typography.bodyLarge)
    Button(onClick = onAllow) { Text(stringResource(R.string.media_gallery_allow)) }
    TextButton(onClick = onPicker) { Text(stringResource(R.string.media_gallery_picker)) }
  }
}

@Composable
private fun Albums(isPartial: Boolean, onMore: () -> Unit, onAlbum: (MediaAlbumRef) -> Unit) {
  val context = LocalContext.current
  val maybeAlbums by produceState<List<MediaAlbum>?>(null) { value = MediaLibrary.albums(context) }
  val albums = maybeAlbums
  if (albums == null) {
    Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) { CircularProgressIndicator() }
    return
  }
  LazyVerticalGrid(
    columns = GridCells.Adaptive(150.dp),
    contentPadding = PaddingValues(16.dp),
    horizontalArrangement = Arrangement.spacedBy(12.dp),
    verticalArrangement = Arrangement.spacedBy(16.dp),
    modifier = Modifier.fillMaxSize(),
  ) {
    if (isPartial) {
      item(span = { GridItemSpan(maxLineSpan) }) {
        TextButton(onClick = onMore) { Text(stringResource(R.string.attach_recent_more)) }
      }
    }
    if (albums.isEmpty()) {
      item(span = { GridItemSpan(maxLineSpan) }) {
        Text(stringResource(R.string.media_gallery_empty), textAlign = TextAlign.Center, modifier = Modifier.padding(32.dp))
      }
    }
    items(albums, key = { "${it.ref.kind}:${it.ref.maybeBucketId}" }) { album ->
      Column(Modifier.clip(RoundedCornerShape(12.dp)).clickable { onAlbum(album.ref) }) {
        Thumbnail(album.cover, side = 180.dp, modifier = Modifier.fillMaxWidth().aspectRatio(1f).clip(RoundedCornerShape(12.dp)))
        Text(albumName(album.ref), style = MaterialTheme.typography.titleSmall, maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.padding(top = 6.dp))
        Text(album.count.toString(), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
      }
    }
  }
}

@Composable
private fun AlbumGrid(album: MediaAlbumRef, draft: MediaDraft, isFull: Boolean, onToggle: (RecentMediaItem) -> Unit) {
  val context = LocalContext.current
  val maybeItems by produceState<List<RecentMediaItem>?>(null, album) { value = MediaLibrary.items(context, album) }
  val items = maybeItems
  if (items == null) {
    Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) { CircularProgressIndicator() }
    return
  }
  val selected = draft.items.map { it.uri }
  LazyVerticalGrid(
    columns = GridCells.Adaptive(88.dp),
    horizontalArrangement = Arrangement.spacedBy(2.dp),
    verticalArrangement = Arrangement.spacedBy(2.dp),
    modifier = Modifier.fillMaxSize(),
  ) {
    items(items, key = { it.uri.toString() }) { item ->
      val number = selected.indexOf(item.uri.toString()) + 1
      GridTile(item, number, isEnabled = number > 0 || !isFull, onToggle = { onToggle(item) })
    }
  }
}

/** One photo or video in an album: a tap selects it with its number, a second tap lets it go. */
@Composable
private fun GridTile(item: RecentMediaItem, number: Int, isEnabled: Boolean, onToggle: () -> Unit) {
  val isSelected = number > 0
  val description = stringResource(if (item.isVideo) R.string.attach_recent_video else R.string.attach_recent_photo)
  Box(
    Modifier
      .aspectRatio(1f)
      .toggleable(value = isSelected, enabled = isEnabled, onValueChange = { onToggle() })
      .semantics { contentDescription = description },
  ) {
    Thumbnail(item, side = 120.dp, modifier = Modifier.matchParentSize())
    if (isSelected) Box(Modifier.matchParentSize().background(Color.Black.copy(alpha = 0.3f)))
    if (item.isVideo) VideoLength(item.durationMs, Modifier.align(Alignment.BottomStart))
    SelectionBadge(number, Modifier.align(Alignment.TopEnd))
  }
}

@Composable
internal fun VideoLength(durationMs: Long, modifier: Modifier) {
  Text(
    DateUtils.formatElapsedTime(durationMs / 1000),
    style = MaterialTheme.typography.labelSmall,
    color = Color.White,
    modifier = modifier.padding(4.dp).background(Color.Black.copy(alpha = 0.5f), RoundedCornerShape(6.dp)).padding(horizontal = 4.dp),
  )
}

@Composable
private fun SelectionBadge(number: Int, modifier: Modifier) {
  val isSelected = number > 0
  Box(
    modifier
      .padding(5.dp)
      .size(24.dp)
      .clip(CircleShape)
      .background(if (isSelected) MaterialTheme.colorScheme.primary else Color.Black.copy(alpha = 0.25f))
      .border(1.5.dp, Color.White, CircleShape),
    contentAlignment = Alignment.Center,
  ) {
    if (isSelected) Text(number.toString(), style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onPrimary)
  }
}

/** The chosen items as a strip, their count, and the button to the review. */
@Composable
private fun SelectionBar(draft: MediaDraft, onNext: () -> Unit) {
  Surface(tonalElevation = 3.dp) {
    Row(Modifier.fillMaxWidth().padding(start = 12.dp, end = 12.dp, top = 8.dp, bottom = 8.dp), verticalAlignment = Alignment.CenterVertically) {
      LazyRow(Modifier.weight(1f), horizontalArrangement = Arrangement.spacedBy(6.dp)) {
        items(draft.items, key = { it.id }) { item ->
          Thumbnail(item.asRecent(), side = 48.dp, modifier = Modifier.size(48.dp).clip(RoundedCornerShape(8.dp)))
        }
      }
      val count = draft.items.size
      val label = pluralStringResource(R.plurals.media_next, count, count)
      // The count beside the arrow, not on it: squeezed into the round button it covered the arrow.
      Button(
        onClick = onNext,
        contentPadding = PaddingValues(horizontal = 16.dp),
        modifier = Modifier.padding(start = 12.dp).height(48.dp).semantics(mergeDescendants = true) { contentDescription = label },
      ) {
        Text(count.toString(), style = MaterialTheme.typography.labelLarge)
        Spacer(Modifier.width(6.dp))
        Icon(painterResource(R.drawable.ic_arrow_forward), contentDescription = null, modifier = Modifier.size(20.dp))
      }
    }
  }
}

internal fun MediaDraftItem.asRecent(): RecentMediaItem = RecentMediaItem(Uri.parse(uri), isVideo, durationMs)

/** A photo's or video's thumbnail, decoded on the device once and kept while the flow is open. */
@Composable
internal fun Thumbnail(item: RecentMediaItem, side: Dp, modifier: Modifier) {
  val context = LocalContext.current
  val sidePx = with(LocalDensity.current) { side.roundToPx() }
  val key = "${item.uri}@$sidePx"
  val maybeThumbnail by produceState(ThumbnailCache.get(key), key) {
    if (value == null) value = loadThumbnail(context, item, sidePx)?.also { ThumbnailCache.put(key, it) }
  }
  Box(modifier.background(MaterialTheme.colorScheme.surfaceContainerHighest)) {
    maybeThumbnail?.let { Image(it, contentDescription = null, contentScale = ContentScale.Crop, modifier = Modifier.matchParentSize()) }
  }
}

private suspend fun loadThumbnail(context: Context, item: RecentMediaItem, sidePx: Int): ImageBitmap? =
  if (item.uri.scheme == "file") {
    // The camera's own files: MediaStore has no thumbnail for them.
    if (item.isVideo) {
      videoFrame(context, item.uri, 0L, sidePx)?.asImageBitmap()
    } else {
      kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) { PictureRenderer.decodeUpright(context.contentResolver, item.uri, sidePx)?.asImageBitmap() }
    }
  } else {
    recentMediaThumbnail(context, item, sidePx)?.asImageBitmap()
  }

/** Decoded thumbnails, bounded in bytes (a sixteenth of the heap). */
private object ThumbnailCache {
  private val cache = object : LruCache<String, ImageBitmap>((Runtime.getRuntime().maxMemory() / 16).toInt()) {
    override fun sizeOf(key: String, value: ImageBitmap): Int = value.width * value.height * 4
  }

  fun get(key: String): ImageBitmap? = cache.get(key)

  fun put(key: String, value: ImageBitmap) {
    cache.put(key, value)
  }
}
