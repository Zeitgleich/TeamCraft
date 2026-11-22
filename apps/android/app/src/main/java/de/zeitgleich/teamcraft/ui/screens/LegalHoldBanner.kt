package de.zeitgleich.teamcraft.ui.screens

import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.LegalHold
import de.zeitgleich.teamcraft.ui.theme.Spacing

/**
 * Every member of a held conversation reads that it is held, with the case reference, and that
 * retention doesn't delete its messages (issue #211, docs/data-export.md §4.2). The hold comes from
 * the owner-signed policy this device verified, so the server can't show it for a conversation
 * nobody held; `maybeHold` is `null` for every other conversation. Desktop and web show the same
 * notice (`LegalHoldNotice`).
 */
@Composable
fun LegalHoldBanner(maybeHold: LegalHold?) {
  val hold = maybeHold ?: return
  Surface(color = MaterialTheme.colorScheme.secondaryContainer, modifier = Modifier.fillMaxWidth()) {
    Row(
      Modifier
        .padding(horizontal = Spacing.l, vertical = Spacing.s)
        .semantics { liveRegion = LiveRegionMode.Polite },
      verticalAlignment = Alignment.CenterVertically,
    ) {
      Icon(
        painterResource(R.drawable.ic_shield),
        contentDescription = null,
        tint = MaterialTheme.colorScheme.onSecondaryContainer,
        modifier = Modifier.size(18.dp),
      )
      Text(
        stringResource(R.string.legal_hold_notice, hold.reference),
        style = MaterialTheme.typography.bodyMedium,
        color = MaterialTheme.colorScheme.onSecondaryContainer,
        modifier = Modifier.weight(1f).padding(start = Spacing.s),
      )
    }
  }
}
