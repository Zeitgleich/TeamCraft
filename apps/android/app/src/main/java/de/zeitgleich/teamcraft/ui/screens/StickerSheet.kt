package de.zeitgleich.teamcraft.ui.screens

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.SecondaryScrollableTabRow
import androidx.compose.material3.Tab
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.State
import androidx.compose.runtime.getValue
import androidx.compose.runtime.produceState
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.TeamCraftApplication
import de.zeitgleich.teamcraft.ui.MainViewModel
import de.zeitgleich.teamcraft.ui.StickerPicture
import de.zeitgleich.teamcraft.ui.Stickers
import de.zeitgleich.teamcraft.ui.theme.Spacing
import kotlinx.coroutines.CancellationException
import uniffi.teamcraft_ffi.StickerItem
import uniffi.teamcraft_ffi.StickerPackItem

/**
 * The sticker sheet (#2132): the workspace's packs as tabs, the chosen pack's stickers as a grid.
 * A tap sends the sticker to the conversation (or the thread) and closes the sheet; a failure
 * shows as the app's failure message.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun StickerSheet(
  viewModel: MainViewModel,
  conversationId: String,
  maybeThread: String?,
  packs: List<StickerPackItem>,
  onDismiss: () -> Unit,
) {
  val context = LocalContext.current
  val graph = TeamCraftApplication.graph(context)
  LaunchedEffect(Unit) {
    // Opening the sheet is when new packs matter: the core reads them unless it did lately.
    try {
      Stickers.want(graph.core.core())
    } catch (cancelled: CancellationException) {
      throw cancelled
    } catch (_: Exception) {
      // Not signed in, or the core closed: the packs held stay.
    }
  }
  var maybeChosenPack by rememberSaveable { mutableStateOf<String?>(null) }
  val maybePack = packs.firstOrNull { it.pack == maybeChosenPack } ?: packs.firstOrNull()
  ModalBottomSheet(onDismissRequest = onDismiss) {
    Column(Modifier.padding(bottom = Spacing.xl)) {
      Text(
        stringResource(R.string.sticker_sheet_title),
        style = MaterialTheme.typography.titleMedium,
        modifier = Modifier.padding(horizontal = Spacing.l, vertical = Spacing.s),
      )
      if (maybePack == null) {
        SheetHint(R.string.sticker_none)
        return@Column
      }
      if (packs.size > 1) {
        SecondaryScrollableTabRow(selectedTabIndex = packs.indexOf(maybePack), edgePadding = Spacing.s) {
          for (pack in packs) {
            Tab(
              selected = pack == maybePack,
              onClick = { maybeChosenPack = pack.pack },
              text = { Text(pack.name, maxLines = 1, overflow = TextOverflow.Ellipsis) },
            )
          }
        }
      }
      if (maybePack.stickers.isEmpty()) {
        SheetHint(R.string.sticker_pack_empty)
        return@Column
      }
      LazyVerticalGrid(
        columns = GridCells.Fixed(STICKER_COLUMNS),
        contentPadding = PaddingValues(horizontal = Spacing.m, vertical = Spacing.s),
        modifier = Modifier.fillMaxWidth().heightIn(max = GRID_MAX_HEIGHT),
      ) {
        items(maybePack.stickers, key = { it.name }) { sticker ->
          StickerCell(sticker) {
            viewModel.sendSticker(conversationId, maybeThread, maybePack.pack, sticker.name, context.getString(R.string.sticker_fallback, sticker.alt))
            onDismiss()
          }
        }
      }
    }
  }
}

/** One sticker of the grid: a square tap target, at least 48 dp, with the picture fitted inside. */
@Composable
private fun StickerCell(sticker: StickerItem, onClick: () -> Unit) {
  Box(
    Modifier
      .padding(Spacing.xs)
      .heightIn(min = MIN_TARGET)
      .aspectRatio(1f)
      .clip(RoundedCornerShape(Spacing.s))
      .clickable(onClickLabel = sticker.alt, role = Role.Button, onClick = onClick)
      .padding(Spacing.xs),
    contentAlignment = Alignment.Center,
  ) {
    StickerPicture(
      sticker.sha256,
      sticker.alt,
      Modifier.fillMaxSize(),
      fallback = {
        // While the picture downloads, its alt text marks the place and still names it for TalkBack.
        Text(
          sticker.alt,
          style = MaterialTheme.typography.labelSmall,
          color = MaterialTheme.colorScheme.onSurfaceVariant,
          maxLines = 2,
          overflow = TextOverflow.Ellipsis,
        )
      },
    )
  }
}

@Composable
private fun SheetHint(text: Int) {
  Text(
    stringResource(text),
    style = MaterialTheme.typography.bodyMedium,
    color = MaterialTheme.colorScheme.onSurfaceVariant,
    modifier = Modifier.padding(horizontal = Spacing.l, vertical = Spacing.xl),
  )
}

/**
 * The verified sticker packs the core holds, read again on each change (a pack or picture that
 * arrives later shows without reopening). Empty when there are none, or no account.
 */
@Composable
fun rememberStickerPacks(): State<List<StickerPackItem>> {
  val graph = TeamCraftApplication.graph(LocalContext.current)
  return produceState(emptyList()) {
    try {
      val core = graph.core.core()
      graph.core.changeCount.collect {
        value = try {
          core.stickerPacks()
        } catch (cancelled: CancellationException) {
          throw cancelled
        } catch (_: Exception) {
          // A read that failed keeps what the sheet showed; the next change reads again.
          value
        }
      }
    } catch (cancelled: CancellationException) {
      throw cancelled
    } catch (_: Exception) {
      // Not signed in, or the core closed: no stickers to offer.
    }
  }
}

private const val STICKER_COLUMNS = 4
private val MIN_TARGET = 48.dp
private val GRID_MAX_HEIGHT = 420.dp
