package de.zeitgleich.teamcraft.ui.screens

import androidx.compose.animation.core.Spring
import androidx.compose.animation.core.animate
import androidx.compose.animation.core.spring
import androidx.compose.animation.core.tween
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.List
import androidx.compose.material.icons.filled.Edit
import androidx.compose.material3.AssistChip
import androidx.compose.material3.AssistChipDefaults
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.VerticalDivider
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.withFrameNanos
import androidx.compose.ui.platform.LocalSoftwareKeyboardController
import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.hapticfeedback.HapticFeedback
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.input.nestedscroll.NestedScrollConnection
import androidx.compose.ui.input.nestedscroll.NestedScrollSource
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.Velocity
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.ui.HomeFilter
import de.zeitgleich.teamcraft.ui.theme.Spacing
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.launch

/** The search field's slot under the header: the 44 dp field and its padding (#2389). */
internal val SEARCH_SLOT_HEIGHT = 60.dp

/** How far the list follows the finger: less than 1:1, so the pull feels like it has weight. */
private const val PULL_RESISTANCE = 0.55f

/** Past the threshold the list still follows, but much more stiffly (a rubber band). */
private const val PULL_OVERSHOOT_RESISTANCE = 0.2f

/** How far past the slot the pull may go, in slots. */
private const val PULL_MAX_SLOTS = 1.6f

/** The release velocity below which a half-shown header snaps by position alone, in px/s. */
private const val HEADER_SNAP_VELOCITY = 600f

/**
 * The home screen's top area (#2389): a header (the top bar, banners and chips) that scrolls away
 * with the list and comes back on scroll up, and the search that a pull down at the top of the list
 * opens. Both move by `graphicsLayer` translations read in the draw phase, never by layout: during
 * a scroll or a pull nothing is measured again. Only opening and closing the search changes the
 * list's top padding, once.
 */
@Stable
internal class HomeTopArea(
  private val scope: CoroutineScope,
  private val haptics: HapticFeedback,
  private val slotPx: Float,
) {
  /** The header's measured height, the list's top padding while the search is closed. */
  var headerHeightPx by mutableFloatStateOf(0f)

  /** How far the header has scrolled away: 0 (shown) to `-headerHeightPx` (gone). */
  var headerOffsetPx by mutableFloatStateOf(0f)
    private set

  /** How far the list is pulled down at its top, in px on screen. */
  var pullPx by mutableFloatStateOf(0f)
    private set

  var isSearchOpen by mutableStateOf(false)
    private set

  var query by mutableStateOf("")

  var chip by mutableStateOf(HomeFilter.All)

  /** False where the shown tab has no search (Activity) or a selection is going on. */
  var isSearchAvailable by mutableStateOf(true)

  /** Reduced motion: the field fades in its slot, nothing moves (motion-audit.md). */
  var isMotionReduced by mutableStateOf(false)

  val focusRequester = FocusRequester()

  private var maybeSettling: Job? = null

  /** How far the field is revealed: 0 hidden, 1 at the threshold, more while overshooting. */
  val pullProgress: Float get() = if (isSearchOpen) 1f else pullPx / slotPx

  /** The list's translation: the pull, or nothing when motion is reduced (the field fades instead). */
  val listTranslationPx: Float get() = if (isMotionReduced) 0f else pullPx

  /** The field's translation below the header: it rides down with the list's top edge. */
  val fieldTranslationPx: Float
    get() = when {
      isSearchOpen || isMotionReduced -> 0f
      else -> pullPx - slotPx
    }

  val fieldAlpha: Float get() = if (isSearchOpen) 1f else pullProgress.coerceIn(0f, 1f)

  /** Brings the header back in full (a new tab, a selection). */
  fun showHeader() {
    headerOffsetPx = 0f
  }

  /** Opens the search from the header's button: as if pulled past the threshold and released. */
  fun open() {
    if (isSearchOpen) return
    headerOffsetPx = 0f
    settle(target = slotPx, velocity = 0f)
  }

  /** Closes the search; the list slides back up under the header. */
  fun close() {
    if (!isSearchOpen) return
    maybeSettling?.cancel()
    query = ""
    isSearchOpen = false
    if (isMotionReduced) {
      pullPx = 0f
      return
    }
    // The list's padding loses the slot in this frame; the same translation keeps it in place.
    pullPx = slotPx
    maybeSettling = scope.launch {
      animate(pullPx, 0f, animationSpec = spring(dampingRatio = Spring.DampingRatioNoBouncy, stiffness = Spring.StiffnessMediumLow)) { value, _ ->
        pullPx = value
      }
    }
  }

  /**
   * The connection the list's container takes: it moves the header by what the list scrolled, and
   * takes the overscroll at the list's top as a pull.
   */
  val connection = object : NestedScrollConnection {
    override fun onPreScroll(available: Offset, source: NestedScrollSource): Offset {
      // Pushing back up while pulled: the pull gives way first, the list doesn't scroll yet.
      if (pullPx > 0f && available.y < 0f && source == NestedScrollSource.UserInput && !isSearchOpen) {
        maybeSettling?.cancel()
        val before = pullPx
        updatePull(rawDelta = available.y)
        return Offset(0f, (pullPx - before) / currentResistance(before))
      }
      return Offset.Zero
    }

    override fun onPostScroll(consumed: Offset, available: Offset, source: NestedScrollSource): Offset {
      if (!isSearchOpen) headerOffsetPx = (headerOffsetPx + consumed.y).coerceIn(-headerHeightPx, 0f)
      val canPull = isSearchAvailable && !isSearchOpen && source == NestedScrollSource.UserInput
      if (canPull && available.y > 0f) {
        maybeSettling?.cancel()
        updatePull(rawDelta = available.y)
        return Offset(0f, available.y)
      }
      return Offset.Zero
    }

    override suspend fun onPreFling(available: Velocity): Velocity {
      if (pullPx <= 0f || isSearchOpen) return Velocity.Zero
      val isPastThreshold = pullPx >= slotPx
      settle(target = if (isPastThreshold) slotPx else 0f, velocity = available.y)
      return available
    }

    override suspend fun onPostFling(consumed: Velocity, available: Velocity): Velocity {
      snapHeader(consumed.y)
      return Velocity.Zero
    }
  }

  /** The search slot's height in dp, for the list's top padding while it is open. */
  fun listTopPadding(headerHeight: Dp): Dp = if (isSearchOpen) headerHeight + SEARCH_SLOT_HEIGHT else headerHeight

  private fun updatePull(rawDelta: Float) {
    val wasPast = pullPx >= slotPx
    val resistance = currentResistance(pullPx)
    pullPx = (pullPx + rawDelta * resistance).coerceIn(0f, slotPx * PULL_MAX_SLOTS)
    val isPast = pullPx >= slotPx
    // One tick where releasing starts to open the search, the way a switch clicks into place.
    if (isPast && !wasPast) haptics.performHapticFeedback(HapticFeedbackType.GestureThresholdActivate)
  }

  private fun currentResistance(pull: Float): Float = if (pull >= slotPx) PULL_OVERSHOOT_RESISTANCE else PULL_RESISTANCE

  /**
   * Springs the pull to [target] from the finger's [velocity] (px/s), so the motion continues the
   * gesture; reaching the slot opens the search. A new touch cancels it mid-way.
   */
  private fun settle(target: Float, velocity: Float) {
    maybeSettling?.cancel()
    maybeSettling = scope.launch {
      if (isMotionReduced) {
        pullPx = target
      } else {
        val isOpening = target > 0f
        val spec = spring<Float>(
          dampingRatio = if (isOpening) Spring.DampingRatioLowBouncy else Spring.DampingRatioNoBouncy,
          stiffness = Spring.StiffnessMediumLow,
        )
        animate(pullPx, target, initialVelocity = velocity, animationSpec = spec) { value, _ -> pullPx = value }
      }
      if (target > 0f) {
        // The field takes its slot in the layout in the same frame the translation goes away.
        isSearchOpen = true
        pullPx = 0f
      }
    }
  }

  /** A header left half-way snaps to shown or gone, as a collapsing top app bar does. */
  private suspend fun snapHeader(velocity: Float) {
    val height = headerHeightPx
    val offset = headerOffsetPx
    if (height <= 0f || offset == 0f || offset == -height || isSearchOpen) return
    val target = when {
      velocity > HEADER_SNAP_VELOCITY -> 0f
      velocity < -HEADER_SNAP_VELOCITY -> -height
      offset > -height / 2 -> 0f
      else -> -height
    }
    if (isMotionReduced) {
      headerOffsetPx = target
      return
    }
    animate(offset, target, animationSpec = tween(durationMillis = 180)) { value, _ -> headerOffsetPx = value }
  }
}

@Composable
internal fun rememberHomeTopArea(): HomeTopArea {
  val scope = rememberCoroutineScope()
  val haptics = LocalHapticFeedback.current
  val slotPx = with(LocalDensity.current) { SEARCH_SLOT_HEIGHT.toPx() }
  return remember(scope, haptics, slotPx) { HomeTopArea(scope, haptics, slotPx) }
}

/**
 * The search field in its slot under the header, with Cancel next to it (as iOS's search bar):
 * filters the shown list by name in place.
 */
@Composable
internal fun HomeSearchField(area: HomeTopArea, modifier: Modifier = Modifier) {
  val keyboard = LocalSoftwareKeyboardController.current
  LaunchedEffect(area.isSearchOpen) {
    if (!area.isSearchOpen) return@LaunchedEffect
    // The deferred text field is there two frames after the field first showed (the button's way in).
    repeat(2) { withFrameNanos {} }
    area.focusRequester.requestFocus()
    keyboard?.show()
  }
  Row(
    modifier.fillMaxWidth().height(SEARCH_SLOT_HEIGHT).padding(start = Spacing.l, end = Spacing.s),
    verticalAlignment = Alignment.CenterVertically,
  ) {
    CompactSearchField(
      value = area.query,
      onChange = { area.query = it },
      placeholder = stringResource(R.string.home_search_placeholder),
      modifier = Modifier.weight(1f),
      focusRequester = area.focusRequester,
      // The look on the pull's first frame, the text field on the next: no hitch as the pull starts.
      isFieldDeferred = true,
    )
    if (area.isSearchOpen) {
      TextButton(onClick = area::close) { Text(stringResource(R.string.cancel)) }
    } else {
      Box(Modifier.width(Spacing.s))
    }
  }
}

/** What the compact chip row of Home offers (#2389). */
internal class HomeChipRow(
  val quickAccess: QuickAccess,
  /** Null where the list has no filter chips (the spaces layout's Home). */
  val maybeFilterChannels: Boolean?,
)

/**
 * One row instead of three cards, a field and a row of chips (#2389): Threads and Saved as small
 * chips, Drafts and Scheduled only while they hold something, then the filter chips.
 */
@Composable
internal fun HomeChips(row: HomeChipRow, selected: HomeFilter, onSelect: (HomeFilter) -> Unit, onNavigate: (String) -> Unit) {
  Row(
    Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()).padding(horizontal = Spacing.l),
    horizontalArrangement = Arrangement.spacedBy(Spacing.s),
    verticalAlignment = Alignment.CenterVertically,
  ) {
    QuickChip(stringResource(R.string.home_entry_threads), { Icon(Icons.AutoMirrored.Filled.List, null, Modifier.size(AssistChipDefaults.IconSize)) }) {
      onNavigate("threads")
    }
    QuickChip(stringResource(R.string.home_entry_saved), { Icon(painterResource(R.drawable.ic_bookmark), null, Modifier.size(AssistChipDefaults.IconSize)) }) {
      onNavigate("saved")
    }
    val drafts = row.quickAccess.draftCount
    if (drafts > 0) {
      QuickChip(
        label = stringResource(R.string.home_entry_drafts),
        maybeCount = drafts,
        icon = { Icon(Icons.Filled.Edit, null, Modifier.size(AssistChipDefaults.IconSize)) },
        description = pluralStringResource(R.plurals.quick_drafts_count, drafts, drafts),
      ) { onNavigate("drafts") }
    }
    val scheduled = row.quickAccess.maybeScheduledCount ?: 0
    if (scheduled > 0 || row.quickAccess.needsAttention) {
      QuickChip(
        label = stringResource(R.string.home_entry_scheduled),
        maybeCount = scheduled,
        icon = { Icon(painterResource(R.drawable.ic_schedule), null, Modifier.size(AssistChipDefaults.IconSize)) },
        description = if (row.quickAccess.needsAttention) {
          stringResource(R.string.quick_scheduled_attention)
        } else {
          pluralStringResource(R.plurals.quick_scheduled_count, scheduled, scheduled)
        },
        isHighlighted = row.quickAccess.needsAttention,
      ) { onNavigate("scheduled") }
    }
    val maybeChannels = row.maybeFilterChannels ?: return@Row
    VerticalDivider(Modifier.height(Spacing.l), color = MaterialTheme.colorScheme.outlineVariant)
    val chips = listOf(
      HomeFilter.Unread to R.string.home_filter_unread,
      HomeFilter.People to R.string.home_filter_people,
      HomeFilter.Groups to R.string.home_filter_groups,
    ) + if (maybeChannels) listOf(HomeFilter.Channels to R.string.channels) else emptyList()
    for ((chip, label) in chips) {
      // Pressing the selected chip again shows everything.
      FilterChip(selected = chip == selected, onClick = { onSelect(if (chip == selected) HomeFilter.All else chip) }, label = { Text(stringResource(label)) })
    }
  }
}

@Composable
private fun QuickChip(
  label: String,
  icon: @Composable () -> Unit,
  maybeCount: Int? = null,
  description: String? = null,
  isHighlighted: Boolean = false,
  onClick: () -> Unit,
) {
  val colors = MaterialTheme.colorScheme
  AssistChip(
    onClick = onClick,
    label = {
      Row(horizontalArrangement = Arrangement.spacedBy(Spacing.xs)) {
        Text(label)
        maybeCount?.let { Text("$it", fontWeight = FontWeight.SemiBold) }
      }
    },
    leadingIcon = icon,
    colors = if (isHighlighted) {
      AssistChipDefaults.assistChipColors(containerColor = colors.primaryContainer, labelColor = colors.onPrimaryContainer, leadingIconContentColor = colors.onPrimaryContainer)
    } else {
      AssistChipDefaults.assistChipColors()
    },
    modifier = if (description == null) Modifier else Modifier.semantics { contentDescription = "$label, $description" },
  )
}
