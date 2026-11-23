package de.zeitgleich.teamcraft.ui.screens

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.ui.MainViewModel
import de.zeitgleich.teamcraft.ui.theme.Spacing
import kotlinx.coroutines.launch
import uniffi.teamcraft_ffi.StrictChannelItem
import uniffi.teamcraft_ffi.StrictDeviceItem
import uniffi.teamcraft_ffi.StrictLossKindItem
import uniffi.teamcraft_ffi.StrictReadinessItem
import uniffi.teamcraft_ffi.StrictStepItem

// Strict channels (e2ee.md §4.6.2, issue #1897), as desktop and web show them: the core decides
// what to show and which action to offer (`StrictChannelItem`); these only render it. Nothing shows
// while the core returns no item, so the app claims no protection it doesn't give.

/** The channel's strict mode, read again on every change; [refresh] reads it afresh first. */
@Composable
fun rememberStrictChannel(viewModel: MainViewModel, conversationId: String, refresh: Boolean): StrictChannelItem? {
  val changes by viewModel.changeCount.collectAsState()
  var maybeStrict by remember(conversationId) { mutableStateOf<StrictChannelItem?>(null) }
  LaunchedEffect(conversationId, refresh) { if (refresh) viewModel.refreshStrictChannel(conversationId) }
  LaunchedEffect(conversationId, changes) { maybeStrict = viewModel.strictChannel(conversationId) }
  return maybeStrict
}

/** "Member changes" in a channel's members sheet. */
@Composable
fun StrictChannelSection(viewModel: MainViewModel, conversationId: String, strict: StrictChannelItem) {
  Surface(color = MaterialTheme.colorScheme.surfaceContainerHigh, shape = MaterialTheme.shapes.medium) {
    Column(Modifier.fillMaxWidth().padding(Spacing.m), verticalArrangement = Arrangement.spacedBy(Spacing.s)) {
      Text(
        stringResource(R.string.channel_strict_title),
        style = MaterialTheme.typography.titleSmall,
        modifier = Modifier.semantics { heading() },
      )
      Text(
        stringResource(if (strict.isStrict) R.string.channel_strict_on else R.string.channel_strict_off),
        style = MaterialTheme.typography.bodyMedium,
      )
      StrictHint(stringResource(R.string.channel_strict_hint))
      if (strict.isStrict) {
        for (person in strict.roles) {
          PersonRow(
            person.maybeName,
            stringResource(if (person.isAdmin) R.string.channel_strict_role_admin else R.string.channel_strict_role_moderator),
          )
        }
        if (strict.canTurnOff) {
          ConfirmedStep(
            viewModel,
            conversationId,
            StrictStepItem.TurnOff,
            label = stringResource(R.string.channel_strict_turn_off),
            confirmText = stringResource(R.string.channel_strict_turn_off_confirm),
            isPrimary = false,
          )
        }
        return@Column
      }
      if (!strict.canTurnOn) {
        StrictHint(stringResource(R.string.channel_strict_ask_workspace_admin))
        return@Column
      }
      if (strict.losses.isEmpty()) {
        StrictHint(stringResource(R.string.channel_strict_nobody_loses))
      } else {
        StrictHint(stringResource(R.string.channel_strict_losses))
        for (loss in strict.losses) {
          PersonRow(
            loss.maybeName,
            stringResource(
              when (loss.loss) {
                StrictLossKindItem.CHANGING_MEMBERS -> R.string.channel_strict_loses_changing
                StrictLossKindItem.ADDING_MEMBERS -> R.string.channel_strict_loses_adding
              },
            ),
          )
        }
      }
      if (!strict.hasAdmin) {
        StrictHint(stringResource(R.string.channel_strict_needs_admin), isStatus = true)
        return@Column
      }
      StrictSteps(viewModel, conversationId, strict, strict.maybeReadiness)
    }
  }
}

/** Steps 2 to 4: ask the devices, the devices that block (D6), turning it on. */
@Composable
private fun StrictSteps(
  viewModel: MainViewModel,
  conversationId: String,
  strict: StrictChannelItem,
  maybeReadiness: StrictReadinessItem?,
) {
  val readiness = maybeReadiness ?: return
  if (readiness.isReady) {
    ConfirmedStep(
      viewModel,
      conversationId,
      StrictStepItem.TurnOn,
      label = stringResource(R.string.channel_strict_turn_on),
      confirmText = stringResource(R.string.channel_strict_turn_on_confirm),
      isPrimary = true,
    )
    return
  }
  if (!readiness.hasAsked) {
    StrictHint(stringResource(R.string.channel_strict_ask_hint))
    StepButton(viewModel, conversationId, StrictStepItem.AskReady, stringResource(R.string.channel_strict_ask), isPrimary = true)
    return
  }
  val waiting = readiness.waiting.size
  Text(
    pluralStringResource(R.plurals.channel_strict_waiting, waiting, waiting),
    style = MaterialTheme.typography.bodyMedium,
    modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
  )
  val refreshing = readiness.refreshingCount.toInt()
  if (refreshing > 0) StrictHint(pluralStringResource(R.plurals.channel_strict_refreshing, refreshing, refreshing), isStatus = true)
  for (device in readiness.waiting) BlockerRow(viewModel, conversationId, device, strict.canRemoveBlocker)
}

@Composable
private fun BlockerRow(viewModel: MainViewModel, conversationId: String, device: StrictDeviceItem, canRemove: Boolean) {
  val name = device.maybeName ?: stringResource(R.string.channel_strict_someone)
  Column(Modifier.fillMaxWidth()) {
    PersonRow(device.maybeName, stringResource(R.string.channel_strict_blocker_device))
    if (canRemove) {
      ConfirmedStep(
        viewModel,
        conversationId,
        StrictStepItem.RemoveBlocker(device.userId, device.deviceId),
        label = stringResource(R.string.channel_strict_remove_device),
        confirmText = stringResource(R.string.channel_strict_remove_device_confirm, name),
        isPrimary = false,
      )
    }
  }
}

/** The notices in a channel: this device stopped at a refused change (D4), or a welcome back waits. */
@OptIn(ExperimentalLayoutApi::class)
@Composable
fun StrictChannelNotice(viewModel: MainViewModel, conversationId: String) {
  val strict = rememberStrictChannel(viewModel, conversationId, refresh = false) ?: return
  strict.maybeStop?.let { stop ->
    NoticeSurface(R.drawable.ic_shield) {
      Text(stringResource(R.string.channel_strict_stopped), style = MaterialTheme.typography.labelMedium)
      if (strict.canContinue) {
        FlowRow(horizontalArrangement = Arrangement.spacedBy(Spacing.s)) {
          val signerId = stop.maybeSignerId
          if (signerId != null) {
            val name = stop.maybeSignerName ?: stringResource(R.string.channel_strict_someone)
            ConfirmedStep(
              viewModel,
              conversationId,
              StrictStepItem.Continue(signerId),
              label = stringResource(R.string.channel_strict_remove_and_continue, name),
              confirmText = stringResource(R.string.channel_strict_remove_and_continue_confirm, name),
              isPrimary = false,
            )
          }
          StepButton(viewModel, conversationId, StrictStepItem.Continue(null), stringResource(R.string.channel_strict_continue), isPrimary = false)
        }
      }
    }
  }
  strict.maybeRejoin?.let { rejoin ->
    NoticeSurface(R.drawable.ic_shield) {
      when {
        rejoin.isAccepted -> Text(stringResource(R.string.channel_strict_rejoin_accepted), style = MaterialTheme.typography.labelMedium)
        rejoin.isDue -> {
          val sender = rejoin.maybeSenderName ?: stringResource(R.string.channel_strict_someone)
          Text(stringResource(R.string.channel_strict_rejoin_due, sender), style = MaterialTheme.typography.labelMedium)
          if (rejoin.joinedNames.isNotEmpty()) {
            Text(stringResource(R.string.channel_strict_rejoin_joined, rejoin.joinedNames.joinToString(", ")), style = MaterialTheme.typography.labelMedium)
          }
          if (rejoin.goneNames.isNotEmpty()) {
            Text(stringResource(R.string.channel_strict_rejoin_gone, rejoin.goneNames.joinToString(", ")), style = MaterialTheme.typography.labelMedium)
          }
          StepButton(viewModel, conversationId, StrictStepItem.AcceptRejoin, stringResource(R.string.channel_strict_rejoin), isPrimary = false)
        }
        else -> Text(stringResource(R.string.channel_strict_rejoin_waiting), style = MaterialTheme.typography.labelMedium)
      }
    }
  }
}

@Composable
private fun NoticeSurface(icon: Int, content: @Composable () -> Unit) {
  Surface(color = MaterialTheme.colorScheme.secondaryContainer, modifier = Modifier.fillMaxWidth()) {
    Row(
      Modifier
        .padding(horizontal = Spacing.l, vertical = Spacing.s)
        .semantics { liveRegion = LiveRegionMode.Polite },
      verticalAlignment = Alignment.Top,
      horizontalArrangement = Arrangement.spacedBy(Spacing.s),
    ) {
      Icon(
        painterResource(icon),
        contentDescription = null,
        tint = MaterialTheme.colorScheme.onSecondaryContainer,
        modifier = Modifier.size(18.dp),
      )
      Column(verticalArrangement = Arrangement.spacedBy(Spacing.xs)) { content() }
    }
  }
}

/** A step that runs at once; disabled while it runs, a failure shows as the screen's failure. */
@Composable
private fun StepButton(viewModel: MainViewModel, conversationId: String, step: StrictStepItem, label: String, isPrimary: Boolean) {
  val scope = rememberCoroutineScope()
  var isRunning by remember(step) { mutableStateOf(false) }
  val onClick: () -> Unit = {
    isRunning = true
    scope.launch {
      viewModel.strictStep(conversationId, step)
      isRunning = false
    }
  }
  if (isPrimary) {
    Button(onClick = onClick, enabled = !isRunning) { Text(label) }
  } else {
    OutlinedButton(onClick = onClick, enabled = !isRunning) { Text(label) }
  }
}

/** A step that is confirmed first: it changes who is in the channel or how it is protected. */
@Composable
private fun ConfirmedStep(
  viewModel: MainViewModel,
  conversationId: String,
  step: StrictStepItem,
  label: String,
  confirmText: String,
  isPrimary: Boolean,
) {
  val scope = rememberCoroutineScope()
  var isConfirming by remember(step) { mutableStateOf(false) }
  var isRunning by remember(step) { mutableStateOf(false) }
  if (isPrimary) {
    Button(onClick = { isConfirming = true }, enabled = !isRunning) { Text(label) }
  } else {
    OutlinedButton(onClick = { isConfirming = true }, enabled = !isRunning) { Text(label) }
  }
  if (!isConfirming) return
  AlertDialog(
    onDismissRequest = { if (!isRunning) isConfirming = false },
    text = { Text(confirmText) },
    confirmButton = {
      TextButton(
        enabled = !isRunning,
        onClick = {
          isRunning = true
          scope.launch {
            // The dialog closes once the core took the step; a failure keeps it open.
            if (viewModel.strictStep(conversationId, step)) isConfirming = false
            isRunning = false
          }
        },
      ) { Text(label) }
    },
    dismissButton = { TextButton(enabled = !isRunning, onClick = { isConfirming = false }) { Text(stringResource(R.string.cancel)) } },
  )
}

@Composable
private fun PersonRow(maybeName: String?, detail: String) {
  Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
    Text(
      maybeName ?: stringResource(R.string.channel_strict_someone),
      style = MaterialTheme.typography.bodyLarge,
      maxLines = 1,
      overflow = TextOverflow.Ellipsis,
      modifier = Modifier.weight(1f),
    )
    Text(detail, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
  }
}

@Composable
private fun StrictHint(text: String, isStatus: Boolean = false) {
  Text(
    text,
    style = MaterialTheme.typography.bodySmall,
    color = MaterialTheme.colorScheme.onSurfaceVariant,
    modifier = if (isStatus) Modifier.semantics { liveRegion = LiveRegionMode.Polite } else Modifier,
  )
}
