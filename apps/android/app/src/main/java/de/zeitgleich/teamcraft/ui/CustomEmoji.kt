package de.zeitgleich.teamcraft.ui

import android.graphics.BitmapFactory
import android.util.LruCache
import androidx.compose.foundation.Image
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.text.InlineTextContent
import androidx.compose.foundation.text.appendInlineContent
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.produceState
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.Placeholder
import androidx.compose.ui.text.PlaceholderVerticalAlign
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.unit.TextUnit
import androidx.compose.ui.unit.sp
import de.zeitgleich.teamcraft.TeamCraftApplication
import de.zeitgleich.teamcraft.core.CustomEmojiRef
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.takeWhile
import kotlinx.coroutines.withContext
import uniffi.teamcraft_ffi.CustomEmojiRefItem
import uniffi.teamcraft_ffi.EmojiSegmentItem
import uniffi.teamcraft_ffi.TeamCraft
import uniffi.teamcraft_ffi.customEmojiSegments

/** Workspace emoji (issue #207) on the phone: the core verifies the set and keeps the pictures. */
object CustomEmoji {
  /** Emoji pictures are small (128 px at most) and a workspace's set is bounded, so this holds them all. */
  private const val CACHE_ENTRIES = 256

  /** How often the app asks the core to read the set again; the core paces its own reads too. */
  private const val WANT_EVERY_MS = 30_000L

  /** How many emoji the reaction sheet lists. */
  const val SHEET_LIMIT = 60

  /** Picture size next to text, in text heights. */
  private const val GLYPH_EM = 1.25f

  private val decoded = LruCache<String, ImageBitmap>(CACHE_ENTRIES)
  private var lastWantedMs = 0L

  fun cached(sha256: String): ImageBitmap? = decoded.get(sha256)

  /** Forgets every picture: the account they belong to is gone from this device. */
  fun clear() = decoded.evictAll()

  /** A picture the core holds, decoded once; `null` while it has not downloaded it. */
  suspend fun picture(core: TeamCraft, sha256: String): ImageBitmap? =
    cached(sha256) ?: withContext(Dispatchers.Default) {
      core.customEmojiPicture(sha256)
        ?.let { BitmapFactory.decodeByteArray(it, 0, it.size) }
        ?.asImageBitmap()
        ?.also { decoded.put(sha256, it) }
    }

  /** Asks the core to read the set unless that was asked lately. Fire and forget. */
  suspend fun want(core: TeamCraft, nowMs: Long = System.currentTimeMillis()) {
    synchronized(this) {
      if (nowMs - lastWantedMs < WANT_EVERY_MS) return
      lastWantedMs = nowMs
    }
    try {
      core.wantCustomEmoji()
    } catch (cancelled: CancellationException) {
      throw cancelled
    } catch (_: Exception) {
      // The core retries when asked again; a picture is not worth an error.
    }
  }

  fun refsOf(refs: List<CustomEmojiRef>): List<CustomEmojiRefItem> =
    refs.map { CustomEmojiRefItem(it.shortcode, it.sha256) }

  /**
   * [text] with the `:shortcode:` words of [refs] replaced by one placeholder character each (the
   * picture is drawn there, its name stays as the alternate text), spans kept. The hashes are the
   * ids of the inline content.
   */
  fun withPictures(text: AnnotatedString, refs: List<CustomEmojiRefItem>): Pictured {
    val pieces = customEmojiSegments(text.text, refs)
    if (pieces.none { it is EmojiSegmentItem.Emoji }) return Pictured(text, emptySet())
    val hashes = mutableSetOf<String>()
    var offset = 0
    val built = buildAnnotatedString {
      for (piece in pieces) {
        when (piece) {
          is EmojiSegmentItem.Text -> {
            append(text.subSequence(offset, offset + piece.text.length))
            offset += piece.text.length
          }
          is EmojiSegmentItem.Emoji -> {
            val name = ":${piece.shortcode}:"
            appendInlineContent(piece.sha256, name)
            hashes += piece.sha256
            offset += name.length
          }
        }
      }
    }
    return Pictured(built, hashes)
  }

  /** The inline content that draws [hashes] at the height of text of [fontSize]. */
  fun inlineContent(hashes: Set<String>, fontSize: TextUnit): Map<String, InlineTextContent> {
    val size = (if (fontSize.isSp) fontSize.value else DEFAULT_FONT_SP).sp * GLYPH_EM
    return hashes.associateWith { sha256 ->
      InlineTextContent(Placeholder(size, size, PlaceholderVerticalAlign.TextCenter)) { alternate ->
        CustomEmojiPicture(sha256, alternate, Modifier.fillMaxSize())
      }
    }
  }

  private const val DEFAULT_FONT_SP = 16f
}

/** A message text with its workspace emoji marked for drawing. */
class Pictured(val text: AnnotatedString, val hashes: Set<String>)

/**
 * A workspace emoji's picture from this device's store. Until the core holds it (it downloads after
 * asking), and when it never will (deleted before this device got it), nothing is drawn: the
 * alternate text, the `:shortcode:`, is what TalkBack and a copy get.
 */
@Composable
fun CustomEmojiPicture(
  sha256: String,
  name: String,
  modifier: Modifier = Modifier,
  /** Drawn while the picture is missing: the reactions show the name, text shows nothing. */
  fallback: @Composable () -> Unit = {},
) {
  val graph = TeamCraftApplication.graph(LocalContext.current)
  val maybePicture by produceState(CustomEmoji.cached(sha256), sha256) {
    if (value != null) return@produceState
    try {
      val core = graph.core.core()
      CustomEmoji.want(core)
      // The core reports a change when the picture arrived; look again until it did.
      graph.core.changeCount.takeWhile {
        value = CustomEmoji.picture(core, sha256)
        value == null
      }.collect {}
    } catch (cancelled: CancellationException) {
      throw cancelled
    } catch (_: Exception) {
      // Not signed in, or the core closed: the name stays.
    }
  }
  val picture = maybePicture
  if (picture == null) {
    fallback()
  } else {
    Image(picture, contentDescription = name, contentScale = ContentScale.Fit, modifier = modifier)
  }
}
