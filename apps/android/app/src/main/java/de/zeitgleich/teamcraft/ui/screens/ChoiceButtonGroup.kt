package de.zeitgleich.teamcraft.ui.screens

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.ButtonGroupDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.ToggleButton
import androidx.compose.material3.ToggleButtonDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.rememberTextMeasurer
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.ui.theme.Spacing

/** A toggle button's horizontal content padding on both sides, plus room for the shape's morph. */
private val BUTTON_PADDING = 40.dp

/**
 * One choice out of a few, as Material 3 Expressive's connected button group (issue #454): the
 * selected button morphs to the round shape. When the labels don't fit side by side (long
 * translations, large text) the buttons stack, each at full width, so no label is cut.
 */
@Composable
fun <T> ChoiceButtonGroup(
  choices: List<Pair<T, Int>>,
  selected: T,
  modifier: Modifier = Modifier,
  onSelect: (T) -> Unit,
) {
  val labels = choices.map { (_, label) -> stringResource(label) }
  val haptics = LocalHapticFeedback.current
  val measurer = rememberTextMeasurer()
  val style = MaterialTheme.typography.labelLarge
  val density = LocalDensity.current
  BoxWithConstraints(modifier.fillMaxWidth()) {
    val isStacked = remember(labels, style, density, maxWidth) {
      val widest = labels.maxOf { measurer.measure(it, style).size.width }
      val perButton = with(density) { widest.toDp() } + BUTTON_PADDING
      perButton * labels.size + ButtonGroupDefaults.ConnectedSpaceBetween * (labels.size - 1) > maxWidth
    }
    val button: @Composable (Int, Modifier) -> Unit = { index, buttonModifier ->
      val (value, _) = choices[index]
      ToggleButton(
        checked = selected == value,
        onCheckedChange = {
          if (selected != value) {
            // Material haptics: a tick when a segmented choice changes.
            haptics.performHapticFeedback(HapticFeedbackType.SegmentTick)
            onSelect(value)
          }
        },
        shapes = when {
          isStacked -> ToggleButtonDefaults.shapesFor(ButtonDefaults.MinHeight)
          index == 0 -> ButtonGroupDefaults.connectedLeadingButtonShapes()
          index == choices.lastIndex -> ButtonGroupDefaults.connectedTrailingButtonShapes()
          else -> ButtonGroupDefaults.connectedMiddleButtonShapes()
        },
        // Unselected buttons on the highest container: the default one is nearly the page's
        // background in the light theme.
        colors = ToggleButtonDefaults.colors(containerColor = MaterialTheme.colorScheme.surfaceContainerHighest),
        modifier = buttonModifier.semantics { role = Role.RadioButton },
      ) {
        Text(labels[index], textAlign = TextAlign.Center)
      }
    }
    if (isStacked) {
      Column(Modifier.fillMaxWidth().selectableGroup(), verticalArrangement = Arrangement.spacedBy(Spacing.xs)) {
        choices.indices.forEach { button(it, Modifier.fillMaxWidth()) }
      }
    } else {
      Row(
        Modifier.fillMaxWidth().selectableGroup(),
        horizontalArrangement = Arrangement.spacedBy(ButtonGroupDefaults.ConnectedSpaceBetween),
      ) {
        choices.indices.forEach { button(it, Modifier.weight(1f)) }
      }
    }
  }
}
