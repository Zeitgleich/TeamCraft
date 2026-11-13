package de.zeitgleich.teamcraft.calls

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import uniffi.teamcraft_ffi.LobbyDecision
import uniffi.teamcraft_ffi.LobbyPerson
import uniffi.teamcraft_ffi.LobbyPersonStatus
import uniffi.teamcraft_ffi.MeetingLobby

private val LOBBY_LIST_HEIGHT = 360.dp

/**
 * "2 people are waiting in the lobby" above the call's controls, for its hosts only (#1859): a tap
 * opens the lobby. Nothing while nobody waits.
 */
@Composable
internal fun LobbyBanner(lobby: MeetingLobby?, onOpen: () -> Unit) {
  val waiting = lobby?.people?.size ?: 0
  if (waiting == 0) return
  Surface(
    color = MaterialTheme.colorScheme.secondaryContainer,
    shape = MaterialTheme.shapes.medium,
    modifier = Modifier.fillMaxWidth().semantics { liveRegion = LiveRegionMode.Polite },
  ) {
    Row(
      Modifier.padding(start = 12.dp, end = 4.dp, top = 4.dp, bottom = 4.dp),
      verticalAlignment = Alignment.CenterVertically,
      horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
      Text(
        pluralStringResource(R.plurals.call_lobby_banner, waiting, waiting),
        modifier = Modifier.weight(1f),
        style = MaterialTheme.typography.bodyMedium,
      )
      TextButton(onClick = onOpen) { Text(stringResource(R.string.call_lobby_review)) }
    }
  }
}

/**
 * The lobby of the call's meeting (#1859), as desktop and web show it: each waiting person with
 * "Admit" and "Deny". A guest's row shows the code of their key to compare with the guest's
 * screen; "Admit" waits until the guest's page set up its device.
 */
@Composable
internal fun LobbyDialog(
  lobby: MeetingLobby,
  deciding: LobbyDeciding,
  onDecide: (LobbyPerson, LobbyDecision) -> Unit,
  onDismissFailure: () -> Unit,
  onClose: () -> Unit,
) {
  val isBusy = deciding is LobbyDeciding.Deciding
  AlertDialog(
    onDismissRequest = onClose,
    title = { Text(stringResource(R.string.call_lobby_title)) },
    text = {
      Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        if (lobby.people.isEmpty()) {
          Text(stringResource(R.string.call_lobby_empty), style = MaterialTheme.typography.bodyMedium)
        }
        LazyColumn(Modifier.heightIn(max = LOBBY_LIST_HEIGHT), verticalArrangement = Arrangement.spacedBy(12.dp)) {
          items(lobby.people, key = { it.userId }) { person ->
            LobbyRow(
              person = person,
              isDeciding = (deciding as? LobbyDeciding.Deciding)?.userId == person.userId,
              isEnabled = !isBusy,
              onDecide = { decision -> onDecide(person, decision) },
            )
          }
        }
        (deciding as? LobbyDeciding.Failed)?.let { failed ->
          Text(
            stringResource(R.string.call_lobby_failed, failed.name, stringResource(failed.reason)),
            color = MaterialTheme.colorScheme.error,
            style = MaterialTheme.typography.bodySmall,
            modifier = Modifier.semantics { liveRegion = LiveRegionMode.Assertive },
          )
        }
      }
    },
    confirmButton = {
      TextButton(onClick = {
        onDismissFailure()
        onClose()
      }) { Text(stringResource(R.string.close)) }
    },
  )
}

@Composable
private fun LobbyRow(person: LobbyPerson, isDeciding: Boolean, isEnabled: Boolean, onDecide: (LobbyDecision) -> Unit) {
  Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(4.dp)) {
    Text(person.displayName, style = MaterialTheme.typography.bodyLarge, maxLines = 1, overflow = TextOverflow.Ellipsis)
    when (val status = person.status) {
      is LobbyPersonStatus.Guest -> {
        val code = status.maybeKeyCode
        val label = stringResource(R.string.call_lobby_guest_code)
        Text(
          if (code != null) "$label $code" else stringResource(R.string.call_lobby_guest_verifying),
          style = MaterialTheme.typography.bodySmall,
          fontFamily = if (code != null) FontFamily.Monospace else null,
          color = MaterialTheme.colorScheme.onSurfaceVariant,
          modifier = if (code != null) Modifier.semantics { contentDescription = "$label ${code.toList().joinToString(" ")}" } else Modifier,
        )
      }
      LobbyPersonStatus.GuestDevicePending -> Text(
        stringResource(R.string.call_lobby_guest_pending),
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
      )
      LobbyPersonStatus.Member -> Unit
    }
    Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
      Button(onClick = { onDecide(LobbyDecision.ADMIT) }, enabled = isEnabled && canAdmit(person)) {
        Text(stringResource(R.string.call_lobby_admit))
      }
      OutlinedButton(onClick = { onDecide(LobbyDecision.DENY) }, enabled = isEnabled) {
        Text(stringResource(R.string.call_lobby_deny))
      }
      if (isDeciding) CircularProgressIndicator(Modifier.size(16.dp), strokeWidth = 2.dp)
    }
  }
}
