package de.zeitgleich.teamcraft.ui.screens

import androidx.annotation.StringRes
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemColors
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.ListItemShapes
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.RadioButton
import androidx.compose.material3.SegmentedListItem
import androidx.compose.material3.Surface
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.toggleableState
import androidx.compose.ui.state.ToggleableState
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.ui.theme.Spacing

/**
 * A group of settings as Google's apps show them with Material 3 Expressive (issue #454): an
 * optional title, then the rows as segments of one rounded container (Material's segmented list
 * items, 2 dp apart, large corners outside, small ones between rows), no dividers. Rows are added
 * in [build]; a row left out (a setting that doesn't apply) closes the gap.
 */
@Composable
fun SettingsGroup(
  @StringRes maybeTitle: Int? = null,
  modifier: Modifier = Modifier,
  maybeFooter: String? = null,
  build: SettingsGroupScope.() -> Unit,
) {
  val rows = SettingsGroupScope().apply(build).rows
  if (rows.isEmpty()) return
  Column(modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(ListItemDefaults.SegmentedGap)) {
    maybeTitle?.let {
      Text(
        stringResource(it),
        style = MaterialTheme.typography.labelLarge,
        color = MaterialTheme.colorScheme.primary,
        modifier = Modifier.padding(start = Spacing.l, bottom = Spacing.s).semantics { heading() },
      )
    }
    rows.forEachIndexed { index, row -> row(ListItemDefaults.segmentedShapes(index = index, count = rows.size)) }
    maybeFooter?.let {
      Text(
        it,
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.padding(horizontal = Spacing.l, vertical = Spacing.xs),
      )
    }
  }
}

/** Collects the rows of a [SettingsGroup]; each row gets its segment's shapes. */
class SettingsGroupScope internal constructor() {
  internal val rows = mutableListOf<@Composable (ListItemShapes) -> Unit>()

  fun row(content: @Composable (ListItemShapes) -> Unit) {
    rows += content
  }
}

/** The segments' colour: the cards of the settings pages, on the page's background. */
@Composable
fun settingsRowColors(): ListItemColors = ListItemDefaults.segmentedColors(containerColor = MaterialTheme.colorScheme.surface)

/** A switch setting; the whole row toggles it, with Material's toggle haptics. */
@Composable
fun SettingsSwitchRow(
  shapes: ListItemShapes,
  title: String,
  isOn: Boolean,
  maybeSummary: String? = null,
  isEnabled: Boolean = true,
  onChange: (Boolean) -> Unit,
) {
  val haptics = LocalHapticFeedback.current
  // A plain clickable row with switch semantics: the list's "checked" item would tint the whole row
  // as selected, which Google's settings don't do for switches.
  SegmentedListItem(
    onClick = {
      haptics.performHapticFeedback(if (!isOn) HapticFeedbackType.ToggleOn else HapticFeedbackType.ToggleOff)
      onChange(!isOn)
    },
    modifier = Modifier.semantics {
      role = Role.Switch
      toggleableState = ToggleableState(isOn)
    },
    shapes = shapes,
    enabled = isEnabled,
    colors = settingsRowColors(),
    supportingContent = maybeSummary?.let { { Text(it) } },
    trailingContent = { Switch(checked = isOn, onCheckedChange = null, enabled = isEnabled) },
  ) { Text(title) }
}

/**
 * A setting with one of a few [choices], each a string resource: the row shows the current one; a
 * tap opens a dialog with the choices as radio buttons and the [maybeHint] that explains them, as
 * Google's apps do.
 */
@Composable
fun <T> SettingsChoiceRow(
  shapes: ListItemShapes,
  title: String,
  choices: List<Pair<T, Int>>,
  selected: T,
  maybeHint: String? = null,
  isEnabled: Boolean = true,
  onSelect: (T) -> Unit,
) {
  val labels = choices.toMap()
  SettingsChoiceRow(
    shapes,
    title,
    choices.map { it.first },
    selected,
    label = { stringResource(labels.getValue(it)) },
    maybeHint = maybeHint,
    isEnabled = isEnabled,
    onSelect = onSelect,
  )
}

/** A [SettingsChoiceRow] whose labels are built ([label]), e.g. with a number in them. */
@Composable
fun <T> SettingsChoiceRow(
  shapes: ListItemShapes,
  title: String,
  choices: List<T>,
  selected: T,
  label: @Composable (T) -> String,
  maybeHint: String? = null,
  isEnabled: Boolean = true,
  onSelect: (T) -> Unit,
) {
  var isOpen by rememberSaveable { mutableStateOf(false) }
  SettingsLinkRow(
    shapes,
    title,
    maybeSummary = selected.takeIf { it in choices }?.let { label(it) },
    isEnabled = isEnabled,
    onClick = { isOpen = true },
  )
  if (isOpen) {
    ChoiceDialog(
      title = title,
      choices = choices,
      selected = selected,
      label = label,
      maybeHint = maybeHint,
      onSelect = {
        isOpen = false
        if (it != selected) onSelect(it)
      },
      onDismiss = { isOpen = false },
    )
  }
}

/** A row that opens something: a page, a dialog, a system screen. */
@Composable
fun SettingsLinkRow(
  shapes: ListItemShapes,
  title: String,
  maybeSummary: String? = null,
  isEnabled: Boolean = true,
  leadingContent: (@Composable () -> Unit)? = null,
  trailingContent: (@Composable () -> Unit)? = null,
  summaryContent: (@Composable () -> Unit)? = maybeSummary?.let { { Text(it) } },
  onClick: () -> Unit,
) {
  SegmentedListItem(
    onClick = onClick,
    shapes = shapes,
    enabled = isEnabled,
    colors = settingsRowColors(),
    leadingContent = leadingContent,
    trailingContent = trailingContent,
    supportingContent = summaryContent,
  ) { Text(title) }
}

/** A row that only shows something (a fingerprint, a status). */
@Composable
fun SettingsInfoRow(shapes: ListItemShapes, title: String, maybeSummary: String? = null, leadingContent: (@Composable () -> Unit)? = null) {
  Surface(shape = shapes.shape, color = MaterialTheme.colorScheme.surface) {
    ListItem(
      supportingContent = maybeSummary?.let { { Text(it) } },
      leadingContent = leadingContent,
      colors = ListItemDefaults.colors(containerColor = MaterialTheme.colorScheme.surface),
    ) { Text(title) }
  }
}

/** A segment with free content (pickers, forms) in the group's container. */
@Composable
fun SettingsContentRow(shapes: ListItemShapes, content: @Composable () -> Unit) {
  Surface(shape = shapes.shape, color = MaterialTheme.colorScheme.surface, modifier = Modifier.fillMaxWidth()) {
    Column(Modifier.padding(Spacing.l), verticalArrangement = Arrangement.spacedBy(Spacing.m)) { content() }
  }
}

/** The choices of a [SettingsChoiceRow]: a tap picks one and closes the dialog. */
@Composable
private fun <T> ChoiceDialog(
  title: String,
  choices: List<T>,
  selected: T,
  label: @Composable (T) -> String,
  maybeHint: String?,
  onSelect: (T) -> Unit,
  onDismiss: () -> Unit,
) {
  AlertDialog(
    onDismissRequest = onDismiss,
    title = { Text(title) },
    text = {
      Column(Modifier.verticalScroll(rememberScrollState())) {
        maybeHint?.let {
          Text(
            it,
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(bottom = Spacing.s),
          )
        }
        Column(Modifier.selectableGroup()) {
          for (value in choices) {
            val isSelected = value == selected
            ListItem(
              leadingContent = { RadioButton(selected = isSelected, onClick = null) },
              colors = ListItemDefaults.colors(containerColor = MaterialTheme.colorScheme.surfaceContainerHigh),
              modifier = Modifier.selectable(selected = isSelected, role = Role.RadioButton) { onSelect(value) },
            ) { Text(label(value)) }
          }
        }
      }
    },
    confirmButton = {},
    dismissButton = { TextButton(onClick = onDismiss) { Text(stringResource(R.string.cancel)) } },
  )
}
