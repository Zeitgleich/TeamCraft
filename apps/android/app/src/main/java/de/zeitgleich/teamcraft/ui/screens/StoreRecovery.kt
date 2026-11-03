package de.zeitgleich.teamcraft.ui.screens

import androidx.annotation.StringRes
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.size
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import uniffi.teamcraft_ffi.StoreProblem

/**
 * The local data can't be read (issue #414): what happened and the ways out, as on desktop and
 * web. Damaged data is set aside and the device set up again, after a confirmation (nothing is
 * deleted); data from a newer version first asks to update and try again.
 */
@Composable
fun StoreRecoveryScreen(
  problem: StoreProblem,
  @StringRes maybeError: Int?,
  isBusy: Boolean,
  onReset: () -> Unit,
  onRetry: () -> Unit,
  onSignOut: () -> Unit,
) {
  var isConfirmingReset by rememberSaveable { mutableStateOf(false) }
  val isNewer = problem == StoreProblem.NEWER_VERSION
  Centered {
    Icon(
      painterResource(R.drawable.ic_warning),
      contentDescription = null,
      tint = MaterialTheme.colorScheme.error,
      modifier = Modifier.size(48.dp),
    )
    Title(if (isNewer) R.string.store_newer_title else R.string.store_damaged_title)
    Text(
      stringResource(if (isNewer) R.string.store_newer_body else R.string.store_damaged_body),
      style = MaterialTheme.typography.bodyMedium,
    )
    maybeError?.let { ErrorLine(it) }
    // The likely way out leads: setting up again for damaged data, updating for newer data.
    if (isNewer) {
      Button(onClick = onRetry, enabled = !isBusy, modifier = Modifier.fillMaxWidth()) {
        Text(stringResource(R.string.retry))
      }
      OutlinedButton(onClick = { isConfirmingReset = true }, enabled = !isBusy, modifier = Modifier.fillMaxWidth()) {
        Text(stringResource(R.string.store_reset))
      }
    } else {
      Button(onClick = { isConfirmingReset = true }, enabled = !isBusy, modifier = Modifier.fillMaxWidth()) {
        Text(stringResource(R.string.store_reset))
      }
      OutlinedButton(onClick = onRetry, enabled = !isBusy, modifier = Modifier.fillMaxWidth()) {
        Text(stringResource(R.string.retry))
      }
    }
    TextButton(onClick = onSignOut, enabled = !isBusy) {
      Text(stringResource(R.string.sign_out), color = MaterialTheme.colorScheme.error)
    }
  }
  if (isConfirmingReset) {
    AlertDialog(
      onDismissRequest = { isConfirmingReset = false },
      title = { Text(stringResource(R.string.store_reset_confirm_title)) },
      text = { Text(stringResource(R.string.store_reset_confirm_message)) },
      confirmButton = {
        TextButton(onClick = {
          isConfirmingReset = false
          onReset()
        }) { Text(stringResource(R.string.store_reset_confirm_button), color = MaterialTheme.colorScheme.error) }
      },
      dismissButton = {
        TextButton(onClick = { isConfirmingReset = false }) { Text(stringResource(R.string.cancel)) }
      },
    )
  }
}
