package de.zeitgleich.teamcraft.ui.screens

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.IntrinsicSize
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.SideEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.ComposerLinkPreview
import de.zeitgleich.teamcraft.core.LinkPreview
import de.zeitgleich.teamcraft.ui.THUMBNAIL_DECODE_PX
import de.zeitgleich.teamcraft.ui.rememberThumbnail

/** The composer's link preview thumbnail: small, so the card stays one compact row. */
private val THUMB_SIZE = 44.dp

/**
 * The link preview under the composer's draft (#2073): what the message will carry, made by the
 * core while the user types. A loading card names the site; the close button takes the preview
 * away, and the message then goes without one. Nothing shows without a link or a preview.
 */
@Composable
internal fun ComposerLinkPreviewCard(preview: ComposerLinkPreview, onRemove: () -> Unit) {
  val isVisible = preview.maybeReady != null || preview.isLoading
  // The card fades out with what it showed, not as an empty loading card.
  val lastVisible = remember { mutableStateOf(preview) }
  SideEffect { if (isVisible) lastVisible.value = preview }
  val shown = if (isVisible) preview else lastVisible.value
  val maybeReady = shown.maybeReady
  AnimatedVisibility(visible = isVisible, enter = fadeIn(), exit = fadeOut()) {
    val colors = MaterialTheme.colorScheme
    Row(
      Modifier
        .fillMaxWidth()
        .padding(horizontal = 12.dp, vertical = 4.dp)
        .clip(RoundedCornerShape(12.dp))
        .background(colors.surfaceContainerHigh)
        .height(IntrinsicSize.Min)
        .semantics { liveRegion = LiveRegionMode.Polite },
      verticalAlignment = Alignment.CenterVertically,
    ) {
      Box(Modifier.width(3.dp).fillMaxHeight().background(if (maybeReady != null) colors.primary else colors.outlineVariant))
      Row(
        Modifier.weight(1f).padding(start = 8.dp, top = 8.dp, bottom = 8.dp),
        horizontalArrangement = Arrangement.spacedBy(10.dp),
        verticalAlignment = Alignment.CenterVertically,
      ) {
        if (maybeReady != null) ReadyContent(maybeReady) else LoadingContent(shown.url.orEmpty())
      }
      IconButton(onClick = onRemove) {
        Icon(Icons.Filled.Close, contentDescription = stringResource(R.string.composer_link_preview_remove))
      }
    }
  }
}

@Composable
private fun RowScope.ReadyContent(preview: LinkPreview) {
  val maybeImage by rememberThumbnail("composer-link:${preview.url}", preview.maybeImage?.bytes, THUMBNAIL_DECODE_PX)
  val image = maybeImage
  if (image != null) {
    Image(
      image,
      contentDescription = null,
      modifier = Modifier.size(THUMB_SIZE).clip(RoundedCornerShape(8.dp)),
      contentScale = ContentScale.Crop,
    )
  } else {
    LinkGlyph()
  }
  Column(Modifier.weight(1f)) {
    val site = preview.maybeSiteName ?: hostOf(preview.url)
    Text(site, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 1, overflow = TextOverflow.Ellipsis)
    Text(
      preview.maybeTitle ?: preview.url,
      style = MaterialTheme.typography.titleSmall,
      maxLines = 1,
      overflow = TextOverflow.Ellipsis,
    )
    preview.maybeDescription?.let {
      Text(it, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 1, overflow = TextOverflow.Ellipsis)
    }
  }
}

@Composable
private fun RowScope.LoadingContent(url: String) {
  LinkGlyph()
  Column(Modifier.weight(1f)) {
    Text(hostOf(url), style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 1, overflow = TextOverflow.Ellipsis)
    Text(
      stringResource(R.string.composer_link_preview_loading),
      style = MaterialTheme.typography.bodySmall,
      color = MaterialTheme.colorScheme.onSurfaceVariant,
      maxLines = 1,
    )
  }
}

@Composable
private fun LinkGlyph() {
  Box(
    Modifier.size(THUMB_SIZE).clip(RoundedCornerShape(8.dp)).background(MaterialTheme.colorScheme.surfaceContainerHighest),
    contentAlignment = Alignment.Center,
  ) {
    Icon(painterResource(R.drawable.ic_link), contentDescription = null, tint = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.size(20.dp))
  }
}

/** The host of a web link, to name the site (`https://user@docs.example.com:8443/a` → `docs.example.com`). */
internal fun hostOf(url: String): String =
  runCatching { java.net.URI(url).host }.getOrNull()?.takeIf { it.isNotEmpty() } ?: url
