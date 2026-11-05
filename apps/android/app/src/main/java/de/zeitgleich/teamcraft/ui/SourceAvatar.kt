package de.zeitgleich.teamcraft.ui

import android.graphics.BitmapFactory
import android.util.LruCache
import androidx.compose.foundation.Image
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.produceState
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import de.zeitgleich.teamcraft.TeamCraftApplication
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/** A channel's sources are few and each picture is small; this keeps scrolling from decoding them again. */
private val decodedPictures = LruCache<String, ImageBitmap>(64)

/**
 * An integration source's picture on its messages (docs/bridge.md, "Source identity"), from this
 * device's encrypted store: no network. [placeholder] (the bot's picture or the source's initial)
 * shows while it loads and when this device doesn't hold it.
 */
@Composable
fun SourceAvatar(conversationId: String, sha256: String, modifier: Modifier = Modifier, placeholder: @Composable () -> Unit) {
  val graph = TeamCraftApplication.graph(LocalContext.current)
  val key = "$conversationId/$sha256"
  val maybePicture by produceState(decodedPictures.get(key), key) {
    if (value == null) {
      value = withContext(Dispatchers.IO) {
        try {
          graph.core.core().integrationSourcePicture(conversationId, sha256)
            ?.let { BitmapFactory.decodeByteArray(it, 0, it.size) }
            ?.asImageBitmap()
            ?.also { decodedPictures.put(key, it) }
        } catch (cancelled: CancellationException) {
          throw cancelled
        } catch (_: Exception) {
          // The placeholder stays: this is a picture, nothing depends on it.
          null
        }
      }
    }
  }
  val picture = maybePicture
  if (picture == null) {
    placeholder()
  } else {
    Box(modifier, contentAlignment = Alignment.Center) {
      Image(picture, contentDescription = null, contentScale = ContentScale.Crop, modifier = Modifier.matchParentSize().clip(CircleShape))
    }
  }
}
