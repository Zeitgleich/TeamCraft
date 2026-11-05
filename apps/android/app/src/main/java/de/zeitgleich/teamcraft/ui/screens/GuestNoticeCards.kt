package de.zeitgleich.teamcraft.ui.screens

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.ExpiringGuest
import de.zeitgleich.teamcraft.ui.theme.Spacing
import java.text.DateFormat
import java.util.Date

/**
 * Guest memberships ending soon, at the top of Activity (issue #212): the guest's own, and for the
 * sponsor and the space's admins the ones they can extend.
 */
@Composable
fun GuestNoticeSection(guests: List<ExpiringGuest>, onExtend: (ExpiringGuest, Long) -> Unit) {
  val dates = DateFormat.getDateInstance(DateFormat.MEDIUM)
  Column(Modifier.fillMaxWidth().padding(bottom = Spacing.s)) {
    Text(
      stringResource(R.string.guest_notice_title).uppercase(),
      style = MaterialTheme.typography.labelMedium,
      color = MaterialTheme.colorScheme.onSurfaceVariant,
      modifier = Modifier.padding(horizontal = Spacing.l, vertical = Spacing.s).semantics { heading() },
    )
    guests.forEach { guest ->
      val date = dates.format(Date(guest.expiresAt))
      Column(Modifier.fillMaxWidth().padding(horizontal = Spacing.l, vertical = Spacing.s)) {
        if (guest.isOwn) {
          Text(stringResource(R.string.guest_notice_self, guest.spaceName, date))
        } else {
          Text(stringResource(R.string.guest_notice_row, guest.user.displayName, guest.spaceName, date))
          Row(horizontalArrangement = Arrangement.spacedBy(Spacing.s)) {
            guest.extendOffers.forEach { offer ->
              OutlinedButton(onClick = { onExtend(guest, offer.until) }) {
                val days = offer.days
                Text(
                  if (days != null) {
                    pluralStringResource(R.plurals.guest_notice_extend, days, days)
                  } else {
                    stringResource(R.string.guest_notice_extend_until, dates.format(Date(offer.until)))
                  },
                )
              }
            }
          }
        }
      }
    }
  }
}
