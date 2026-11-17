package de.zeitgleich.teamcraft.ui.screens

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.ui.theme.Spacing
import java.text.DateFormat
import java.util.Date
import uniffi.teamcraft_ffi.InactivityNoticeItem
import uniffi.teamcraft_ffi.InactivityNoticeKindItem

/**
 * What the owners of a quiet channel are told in the channel (#212): nobody wrote in it for a
 * while, when it is archived or deleted, and "keep open". The core decides what is said and to
 * whom (`maybeNotice` is `null` for everybody else); this only draws it.
 */
@Composable
fun InactivityNoticeBanner(maybeNotice: InactivityNoticeItem?, onKeepOpen: () -> Unit) {
  val notice = maybeNotice ?: return
  val dates = DateFormat.getDateInstance(DateFormat.MEDIUM)
  val maybeDate = notice.maybeAtMs?.let { dates.format(Date(it)) }
  val text = when (notice.kind) {
    InactivityNoticeKindItem.QUIET ->
      if (maybeDate != null) stringResource(R.string.inactivity_notice_archive_on, maybeDate)
      else stringResource(R.string.inactivity_notice_quiet)
    InactivityNoticeKindItem.ARCHIVED_BY_POLICY ->
      if (maybeDate != null) stringResource(R.string.inactivity_notice_delete_on, maybeDate)
      else stringResource(R.string.inactivity_notice_archived)
    InactivityNoticeKindItem.KEPT_OPEN -> stringResource(R.string.inactivity_notice_kept_open, maybeDate.orEmpty())
  }
  Surface(color = MaterialTheme.colorScheme.secondaryContainer, modifier = Modifier.fillMaxWidth()) {
    Column(
      Modifier
        .padding(horizontal = Spacing.l, vertical = Spacing.s)
        .semantics { liveRegion = LiveRegionMode.Polite },
      verticalArrangement = Arrangement.spacedBy(Spacing.s),
    ) {
      Text(text, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSecondaryContainer)
      if (notice.offersKeepOpen) {
        OutlinedButton(onClick = onKeepOpen) {
          Text(
            stringResource(
              if (notice.kind == InactivityNoticeKindItem.ARCHIVED_BY_POLICY) {
                R.string.inactivity_notice_postpone
              } else {
                R.string.inactivity_notice_keep
              },
            ),
          )
        }
      }
    }
  }
}
