package de.zeitgleich.teamcraft.ui

import android.graphics.BitmapFactory
import android.util.LruCache
import androidx.compose.foundation.Image
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.produceState
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import de.zeitgleich.teamcraft.TeamCraftApplication
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.takeWhile
import kotlinx.coroutines.withContext
import uniffi.teamcraft_ffi.TeamCraft

/** Stickers (issue #2132) on the phone: the core verifies the packs and keeps the pictures. */
object Stickers {
  /**
   * Decoded pictures kept in memory. Stickers are bigger than emoji (512 px at most), so this holds
   * what a sheet and a screen of messages show, not every pack.
   */
  private const val CACHE_ENTRIES = 128

  /** How often the app asks the core to read the packs again; the core paces its own reads too. */
  private const val WANT_EVERY_MS = 30_000L

  private val decoded = LruCache<String, ImageBitmap>(CACHE_ENTRIES)
  private var lastWantedMs = 0L

  fun cached(sha256: String): ImageBitmap? = decoded.get(sha256)

  /** Forgets every picture: the account they belong to is gone from this device. */
  fun clear() = decoded.evictAll()

  /** A picture the core holds, decoded once off the main thread; `null` while it has not downloaded it. */
  suspend fun picture(core: TeamCraft, sha256: String): ImageBitmap? =
    cached(sha256) ?: withContext(Dispatchers.Default) {
      core.stickerPicture(sha256)
        ?.let { BitmapFactory.decodeByteArray(it, 0, it.size) }
        ?.asImageBitmap()
        ?.also { decoded.put(sha256, it) }
    }

  /** Asks the core to read the packs unless that was asked lately. Fire and forget. */
  suspend fun want(core: TeamCraft, nowMs: Long = System.currentTimeMillis()) {
    synchronized(this) {
      if (nowMs - lastWantedMs < WANT_EVERY_MS) return
      lastWantedMs = nowMs
    }
    try {
      core.wantStickerPacks()
    } catch (cancelled: CancellationException) {
      throw cancelled
    } catch (_: Exception) {
      // The core retries when asked again; a picture is not worth an error.
    }
  }
}

/**
 * A sticker's picture from this device's store, fitted into [modifier]'s size. Until the core holds
 * it (it downloads after asking), and when it never will, [fallback] is drawn instead: a message
 * shows its fallback text there.
 */
@Composable
fun StickerPicture(
  sha256: String,
  alt: String,
  modifier: Modifier = Modifier,
  fallback: @Composable () -> Unit = {},
) {
  val graph = TeamCraftApplication.graph(LocalContext.current)
  val maybePicture by produceState(Stickers.cached(sha256), sha256) {
    if (value != null) return@produceState
    try {
      val core = graph.core.core()
      Stickers.want(core)
      // The core reports a change when the picture arrived; look again until it did.
      graph.core.changeCount.takeWhile {
        value = Stickers.picture(core, sha256)
        value == null
      }.collect {}
    } catch (cancelled: CancellationException) {
      throw cancelled
    } catch (_: Exception) {
      // Not signed in, or the core closed: the fallback stays.
    }
  }
  val picture = maybePicture
  if (picture == null) {
    fallback()
  } else {
    Image(picture, contentDescription = alt, contentScale = ContentScale.Fit, modifier = modifier)
  }
}
