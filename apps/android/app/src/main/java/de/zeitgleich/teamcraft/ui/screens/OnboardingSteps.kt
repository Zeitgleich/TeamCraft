package de.zeitgleich.teamcraft.ui.screens

import androidx.annotation.DrawableRes
import androidx.annotation.StringRes
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Icon
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.SegmentedListItem
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.paneTitle
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.ui.theme.Spacing

/** Steps keep a readable line length on tablets and in landscape. */
private val STEP_MAX_WIDTH = 560.dp

/**
 * One step of the guided first run (issue #1701), laid out like Android's own setup: the step
 * indicator and the heading on top, the step's content scrolling below, and the actions in a bar
 * at the bottom (in thumb reach, above the keyboard), the main one on the right.
 *
 * [paneTitle] makes TalkBack announce each new step's title when the step changes.
 */
@Composable
internal fun StepPage(
  step: Int,
  steps: Int,
  title: String,
  @StringRes maybeLead: Int?,
  actions: @Composable StepActionsScope.() -> Unit,
  content: @Composable ColumnScope.() -> Unit,
) {
  Surface(Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.surface) {
    Column(Modifier.safeDrawingPadding().fillMaxSize(), horizontalAlignment = Alignment.CenterHorizontally) {
      Column(
        modifier = Modifier
          .weight(1f)
          .widthIn(max = STEP_MAX_WIDTH)
          .fillMaxWidth()
          .verticalScroll(rememberScrollState())
          .padding(horizontal = Spacing.xl, vertical = Spacing.l)
          .semantics { paneTitle = title },
        verticalArrangement = Arrangement.spacedBy(Spacing.l),
      ) {
        StepProgress(step, steps)
        Text(
          title,
          style = MaterialTheme.typography.headlineMedium,
          modifier = Modifier.semantics { heading() },
        )
        maybeLead?.let {
          Text(stringResource(it), style = MaterialTheme.typography.bodyLarge, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        content()
      }
      StepActions(actions)
    }
  }
}

/** The bar's buttons: [secondary] is a text button on the left, [primary] the filled one. */
class StepActionsScope internal constructor() {
  internal var maybeSecondary: (@Composable () -> Unit)? = null
  internal var maybePrimary: (@Composable () -> Unit)? = null

  fun secondary(content: @Composable () -> Unit) {
    maybeSecondary = content
  }

  fun primary(content: @Composable () -> Unit) {
    maybePrimary = content
  }
}

/**
 * Back on the left, the main action on the right; with large text they wrap onto two lines
 * instead of squeezing.
 */
@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun StepActions(build: @Composable StepActionsScope.() -> Unit) {
  val scope = StepActionsScope().apply { build() }
  val maybeSecondary = scope.maybeSecondary
  FlowRow(
    modifier = Modifier
      .widthIn(max = STEP_MAX_WIDTH)
      .fillMaxWidth()
      .padding(horizontal = Spacing.xl, vertical = Spacing.l),
    horizontalArrangement = if (maybeSecondary == null) Arrangement.End else Arrangement.SpaceBetween,
    verticalArrangement = Arrangement.spacedBy(Spacing.s),
  ) {
    maybeSecondary?.invoke()
    scope.maybePrimary?.invoke()
  }
}

/** "Step 2 of 3" over a determinate bar; TalkBack reads the text, the bar is decoration. */
@Composable
internal fun StepProgress(step: Int, steps: Int) {
  Column(verticalArrangement = Arrangement.spacedBy(Spacing.s)) {
    LinearProgressIndicator(
      progress = { step.toFloat() / steps },
      modifier = Modifier.fillMaxWidth().clearAndSetSemantics {},
    )
    Text(
      stringResource(R.string.onboarding_step, step, steps),
      style = MaterialTheme.typography.labelLarge,
      color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
  }
}

/** One explained point of a step: an icon in a tinted tile, a short title and a plain sentence. */
@Composable
internal fun StepPoint(@DrawableRes icon: Int, @StringRes title: Int, @StringRes text: Int) {
  Row(horizontalArrangement = Arrangement.spacedBy(Spacing.l)) {
    StepIcon(icon)
    Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
      Text(stringResource(title), style = MaterialTheme.typography.titleMedium)
      Text(stringResource(text), style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
    }
  }
}

/** A way to choose, as one segment of a rounded group (Material 3 Expressive, as in Settings). */
@Composable
internal fun StepChoices(choices: List<StepChoice>) {
  Column(verticalArrangement = Arrangement.spacedBy(ListItemDefaults.SegmentedGap)) {
    choices.forEachIndexed { index, choice ->
      SegmentedListItem(
        onClick = choice.onClick,
        shapes = ListItemDefaults.segmentedShapes(index = index, count = choices.size),
        enabled = choice.isEnabled,
        colors = ListItemDefaults.segmentedColors(containerColor = MaterialTheme.colorScheme.surfaceContainerHigh),
        leadingContent = { StepIcon(choice.icon) },
        trailingContent = { Icon(painterResource(R.drawable.ic_chevron_right), contentDescription = null) },
        overlineContent = if (choice.isRecommended) {
          { Text(stringResource(R.string.onboarding_recommended), color = MaterialTheme.colorScheme.primary) }
        } else {
          null
        },
        supportingContent = { Text(stringResource(choice.text)) },
      ) { Text(stringResource(choice.title)) }
    }
  }
}

/** One row of [StepChoices]. */
internal data class StepChoice(
  @DrawableRes val icon: Int,
  @StringRes val title: Int,
  @StringRes val text: Int,
  val isRecommended: Boolean = false,
  val isEnabled: Boolean = true,
  val onClick: () -> Unit,
)

@Composable
private fun StepIcon(@DrawableRes icon: Int) {
  Surface(shape = RoundedCornerShape(12.dp), color = MaterialTheme.colorScheme.secondaryContainer, modifier = Modifier.size(40.dp)) {
    Box(contentAlignment = Alignment.Center) {
      Icon(painterResource(icon), contentDescription = null, tint = MaterialTheme.colorScheme.onSecondaryContainer, modifier = Modifier.size(22.dp))
    }
  }
}
