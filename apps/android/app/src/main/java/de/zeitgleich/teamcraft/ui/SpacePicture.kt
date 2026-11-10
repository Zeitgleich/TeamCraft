package de.zeitgleich.teamcraft.ui

import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.util.Log
import android.util.LruCache
import androidx.compose.foundation.Image
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.produceState
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.TeamCraftApplication
import java.io.File
import java.util.concurrent.ConcurrentHashMap
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.withContext
import uniffi.teamcraft_ffi.TeamCraft

/**
 * The pictures of spaces (issue #284). A picture is kept as a file named by the space and the
 * version the space list names, so a new version is a new file and the old one is deleted; a space
 * whose picture was removed loses its files. Only spaces on screen ask for theirs ([SpacePicture]),
 * never all of a big workspace at once. The disk holds at most [MAX_FILES] pictures (the least
 * recently written go first), memory the last [MEMORY_ENTRIES] decoded ones. A failed download is
 * asked for again in the next shared round ([PictureRetries]), at once when the device comes back
 * online; until then the screen shows no picture.
 */
class SpacePictureCache(
  private val context: Context,
  private val core: suspend () -> TeamCraft,
  private val retries: PictureRetries,
) {
  private val inFlight = ConcurrentHashMap.newKeySet<String>()
  private val memory = LruCache<String, Bitmap>(MEMORY_ENTRIES)
  private val changeCount = MutableStateFlow(0)

  /** Counts changed pictures: a screen reads its picture again when this moves. */
  val changes: StateFlow<Int> = changeCount

  private var generation = 0
  private val writeLock = Any()

  /** The decoded picture of `spaceId` in `version` if it is in memory; never touches the disk. */
  fun maybeDecoded(spaceId: String, version: String): Bitmap? = memory.get(key(spaceId, version))

  /** The picture from the cache folder; call off the main thread. */
  fun maybeBitmap(spaceId: String, version: String): Bitmap? {
    val key = key(spaceId, version)
    memory.get(key)?.let { return it }
    val file = pictureFile(spaceId, version) ?: return null
    if (!file.isFile) return null
    return decode(file)?.also { memory.put(key, it) }
  }

  suspend fun loadBitmap(spaceId: String, version: String): Bitmap? =
    withContext(Dispatchers.IO) { maybeBitmap(spaceId, version) }

  /** Makes sure the picture in `version` is on disk; `true` when it arrived just now. */
  suspend fun ensure(spaceId: String, version: String): Boolean = withContext(Dispatchers.IO) { fetch(spaceId, version) }

  /** A space without a picture (any more) keeps no file. */
  suspend fun forget(spaceId: String) = withContext(Dispatchers.IO) {
    synchronized(writeLock) {
      if (deleteFilesOf(spaceId, keep = null)) changed()
    }
  }

  private suspend fun fetch(spaceId: String, version: String): Boolean {
    val file = pictureFile(spaceId, version) ?: return false
    if (file.isFile) return false
    val flight = key(spaceId, version)
    if (!inFlight.add(flight)) return false
    val startedIn = synchronized(writeLock) { generation }
    return try {
      val bytes = core().spacePicture(spaceId, MAX_PICTURE_BYTES)
      retries.succeeded()
      synchronized(writeLock) {
        when {
          generation != startedIn -> false
          bytes == null -> {
            deleteFilesOf(spaceId, keep = null)
            changed()
            false
          }
          !isDecodable(bytes) -> false
          else -> {
            file.parentFile?.mkdirs()
            val partial = File(file.parentFile, file.name + ".part")
            partial.writeBytes(bytes)
            // A rename, so a killed app never leaves half a picture under the real name.
            if (!partial.renameTo(file)) return@synchronized false
            deleteFilesOf(spaceId, keep = file)
            trim()
            changed()
            true
          }
        }
      }
    } catch (cancelled: CancellationException) {
      throw cancelled
    } catch (failure: Exception) {
      // Offline, signed out or a storage error: the screen shows no picture. Trouble that passes is
      // asked for again in the next shared round (issue #1616), the rest at the next look.
      Log.w(TAG, "space picture not fetched: ${failure.javaClass.simpleName}")
      if (isTransient(failure)) retries.failed()
      false
    } finally {
      inFlight.remove(flight)
    }
  }

  /** Forgets every picture (sign-out): they belong to the account that is gone. */
  fun clear() {
    synchronized(writeLock) {
      generation++
      memory.evictAll()
      runCatching { directory().deleteRecursively() }
      changeCount.value++
    }
  }

  private fun changed() {
    memory.evictAll()
    changeCount.value++
  }

  /** Deletes the files of `spaceId` except `keep`; answers whether any went. */
  private fun deleteFilesOf(spaceId: String, keep: File?): Boolean {
    val files = directory().listFiles { file -> file.name.startsWith("$spaceId-") && file != keep } ?: return false
    return files.count { it.delete() } > 0
  }

  private fun trim() {
    val files = directory().listFiles { file -> file.name.endsWith(EXTENSION) } ?: return
    if (files.size <= MAX_FILES) return
    files.sortedBy { it.lastModified() }.take(files.size - MAX_FILES).forEach { it.delete() }
  }

  private fun directory() = File(context.cacheDir, DIRECTORY)

  /** Only numeric space IDs and plain versions name a file; anything else has no picture. */
  private fun pictureFile(spaceId: String, version: String): File? {
    if (spaceId.isEmpty() || !spaceId.all(Char::isDigit)) return null
    val safe = version.filter { it.isLetterOrDigit() }.take(MAX_VERSION_CHARS)
    if (safe.isEmpty()) return null
    return File(directory(), "$spaceId-$safe$EXTENSION")
  }

  private fun key(spaceId: String, version: String) = "$spaceId-$version"

  private fun isDecodable(bytes: ByteArray): Boolean {
    val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
    BitmapFactory.decodeByteArray(bytes, 0, bytes.size, bounds)
    return bounds.outWidth in 1..MAX_SIDE_PX && bounds.outHeight in 1..MAX_SIDE_PX
  }

  private fun decode(file: File): Bitmap? {
    val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
    BitmapFactory.decodeFile(file.path, bounds)
    if (bounds.outWidth !in 1..MAX_SIDE_PX || bounds.outHeight !in 1..MAX_SIDE_PX) return null
    var sample = 1
    while (minOf(bounds.outWidth, bounds.outHeight) / (sample * 2) >= DECODE_PX) sample *= 2
    return BitmapFactory.decodeFile(file.path, BitmapFactory.Options().apply { inSampleSize = sample })
  }

  companion object {
    private const val DIRECTORY = "space-pictures"
    private const val EXTENSION = ".png"
    private const val TAG = "TeamCraftSpaces"

    /** The server keeps a picture to 512 px; the list shows it at a fraction of that. */
    private const val DECODE_PX = 96
    private const val MAX_SIDE_PX = 1024
    private const val MAX_VERSION_CHARS = 64
    private const val MAX_FILES = 64
    private const val MEMORY_ENTRIES = 32
    private const val MAX_PICTURE_BYTES = 1_048_576u // unsigned arithmetic is not a constant expression
  }
}

/**
 * A space's picture as a small rounded square, or nothing without one (so the row keeps its current
 * look), while it loads and when it can't be loaded. It is fetched only while composed, which keeps
 * the downloads to the spaces on screen.
 */
@Composable
fun SpacePicture(spaceId: String, maybeVersion: String?, size: Dp = 24.dp, modifier: Modifier = Modifier) {
  val graph = TeamCraftApplication.graph(LocalContext.current)
  val pictures = graph.spacePictures
  val changes by pictures.changes.collectAsState()
  val isOnline by graph.network.isOnline.collectAsState()
  val retryRound by graph.pictureRetries.round.collectAsState()
  val maybePicture by produceState<ImageBitmap?>(
    maybeVersion?.let { pictures.maybeDecoded(spaceId, it) }?.asImageBitmap(),
    spaceId,
    maybeVersion,
    changes,
  ) {
    value = maybeVersion?.let { pictures.loadBitmap(spaceId, it) }?.asImageBitmap()
  }
  LaunchedEffect(spaceId, maybeVersion, isOnline, retryRound) {
    when {
      maybeVersion == null -> pictures.forget(spaceId)
      isOnline -> pictures.ensure(spaceId, maybeVersion)
    }
  }
  val picture = maybePicture ?: return
  Image(
    picture,
    contentDescription = null,
    contentScale = ContentScale.Crop,
    modifier = modifier.size(size).clip(RoundedCornerShape(size / 4)),
  )
}
