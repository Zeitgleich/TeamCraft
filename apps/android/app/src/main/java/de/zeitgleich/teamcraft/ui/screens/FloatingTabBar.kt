package de.zeitgleich.teamcraft.ui.screens

import androidx.compose.animation.animateColorAsState
import androidx.compose.animation.core.animateDpAsState
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.asPaddingValues
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.navigationBars
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Email
import androidx.compose.material.icons.filled.Home
import androidx.compose.material.icons.filled.Notifications
import androidx.compose.material3.Badge
import androidx.compose.material3.BadgedBox
import androidx.compose.material3.FloatingToolbarDefaults
import androidx.compose.material3.HorizontalFloatingToolbar
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.ripple
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.platform.LocalWindowInfo
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.rememberTextMeasurer
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.ui.ChatTab
import de.zeitgleich.teamcraft.ui.TabBadge
import de.zeitgleich.teamcraft.ui.isDirect
import de.zeitgleich.teamcraft.ui.tabBadge
import de.zeitgleich.teamcraft.ui.theme.LocalTeamCraftColors
import uniffi.teamcraft_ffi.ConversationItem

/** Width of a tab that shows only its icon; also the touch target. */
private val ITEM_SIZE = 48.dp
private val ITEM_PADDING = 12.dp
private val ICON_LABEL_GAP = 8.dp
private val ICON_SIZE = 24.dp

/** Shadow under the bar; the tonal level alone is too close to the background in light mode. */
private val BAR_SHADOW = 8.dp

/** The toolbar's content padding on both sides plus the screen margins around it. */
private val TOOLBAR_CHROME = 16.dp + 32.dp

/** Height of the floating bar above the navigation bar, its screen offset included. */
val FLOATING_BAR_HEIGHT = FloatingToolbarDefaults.ContainerSize + FloatingToolbarDefaults.ScreenOffset

/** Room for the main action button above the bar: the button and its gaps. */
private val FAB_ROOM = 56.dp + 16.dp

/**
 * Bottom room for the main screen's lists: the floating bar, the action button above it and the
 * navigation bar beneath, so the lists scroll behind them (edge to edge) and their last row ends
 * clear of them.
 */
@Composable
fun floatingTabBarClearance(): Dp =
  FLOATING_BAR_HEIGHT + FAB_ROOM + WindowInsets.navigationBars.asPaddingValues().calculateBottomPadding()

/**
 * The main screen's navigation as Google's current apps show it with Material 3 Expressive (issue
 * #454): a floating toolbar with Home, DMs and Activity, the selected tab widened to icon and label
 * in a tonal pill (as wide as its own label), the others icon-only with their badges. The main action
 * is the screen's own floating action button above it.
 */
@Composable
fun FloatingTabBar(
  selected: ChatTab,
  isList: Boolean,
  conversations: List<ConversationItem>,
  joinRequestCount: Int,
  onSelect: (ChatTab) -> Unit,
  modifier: Modifier = Modifier,
) {
  val haptics = LocalHapticFeedback.current
  // The list layout has the DMs on Home and no tab of their own for them.
  val tabs = ChatTab.entries.filterNot { isList && it == ChatTab.Direct }
  val labelWidths = rememberSelectedItemWidths(tabs)
  // Narrow windows with a large font: the label would make the pill wider than the window, so the
  // selected tab keeps its highlight but shows its icon only.
  val needed = labelWidths.values.max() + (ITEM_SIZE + 4.dp) * (tabs.size - 1) + TOOLBAR_CHROME
  val showsLabel = needed <= LocalWindowInfo.current.containerDpSize.width
  val (direct, channels) = conversations.partition { it.isDirect }
  // The pill keeps one width whichever tab is selected, so it never wanders sideways (it is
  // centred): it is laid out for the longest label, the selected tab is as wide as its own label,
  // and the rest goes to the icon-only tabs as air around their icons. Selected and other tabs
  // animate with the same spring, so their widths add up to the same total in every frame.
  val selectedWidth = if (showsLabel) labelWidths.getValue(selected) else ITEM_SIZE
  val longestWidth = if (showsLabel) labelWidths.values.max() else ITEM_SIZE
  val otherWidth = ITEM_SIZE + (longestWidth - selectedWidth) / (tabs.size - 1).coerceAtLeast(1)
  // The default container (surfaceContainer) is nearly the screen background in light mode, with
  // dynamic colour too: a higher tonal level and a clearer shadow lift the bar off the lists.
  val colors = FloatingToolbarDefaults.standardFloatingToolbarColors().copy(
    toolbarContainerColor = MaterialTheme.colorScheme.surfaceContainerHighest,
  )
  HorizontalFloatingToolbar(expanded = true, modifier = modifier, colors = colors, expandedShadowElevation = BAR_SHADOW) {
    Row(Modifier.selectableGroup(), horizontalArrangement = Arrangement.spacedBy(4.dp), verticalAlignment = Alignment.CenterVertically) {
      for (tab in tabs) {
        val badge = when (tab) {
          // Channels count as "something new" there too; a number is for what is addressed to the user.
          ChatTab.Home -> if (isList) TabBadge(maxOf(tabBadge(conversations).mentions, tabBadge(direct).unread), tabBadge(channels).unread) else tabBadge(channels)
          ChatTab.Direct -> tabBadge(direct)
          // A request waits for a decision like a mention waits for an answer.
          ChatTab.Activity -> TabBadge(mentions = tabBadge(conversations).mentions + joinRequestCount, unread = 0)
        }
        // Channels and Activity count only mentions; DMs count every unread message.
        val countsUnread = tab == ChatTab.Direct || (isList && tab == ChatTab.Home)
        val isSelected = tab == selected
        TabItem(tab, badge, countsUnread, isSelected, if (isSelected) selectedWidth else otherWidth, showsLabel) {
          if (tab != selected) haptics.performHapticFeedback(HapticFeedbackType.SegmentTick)
          onSelect(tab)
        }
      }
    }
  }
}

/** Each tab's width while selected: its icon and its own label, so the selected pill has no empty space. */
@Composable
private fun rememberSelectedItemWidths(tabs: List<ChatTab>): Map<ChatTab, Dp> {
  val measurer = rememberTextMeasurer()
  val style = MaterialTheme.typography.labelLarge
  val labels = tabs.map { stringResource(tabLabel(it)) }
  val density = LocalDensity.current
  return remember(labels, style, density) {
    tabs.zip(labels).associate { (tab, label) ->
      val width = measurer.measure(label, style, maxLines = 1).size.width
      tab to with(density) { width.toDp() } + ICON_SIZE + ICON_LABEL_GAP + ITEM_PADDING * 2
    }
  }
}

@Composable
private fun TabItem(
  tab: ChatTab,
  badge: TabBadge,
  countsUnread: Boolean,
  isSelected: Boolean,
  targetWidth: Dp,
  showsLabel: Boolean,
  onClick: () -> Unit,
) {
  val motion = MaterialTheme.motionScheme
  val container by animateColorAsState(
    if (isSelected) MaterialTheme.colorScheme.secondaryContainer else MaterialTheme.colorScheme.secondaryContainer.copy(alpha = 0f),
    motion.fastEffectsSpec(),
    label = "tab container",
  )
  val content by animateColorAsState(
    if (isSelected) MaterialTheme.colorScheme.onSecondaryContainer else MaterialTheme.colorScheme.onSurfaceVariant,
    motion.fastEffectsSpec(),
    label = "tab content",
  )
  val width by animateDpAsState(targetWidth, motion.fastSpatialSpec(), label = "tab width")
  // The icon sits at the start of the selected pill and in the middle of an icon-only tab.
  val iconStart by animateDpAsState(
    if (isSelected && showsLabel) ITEM_PADDING else (targetWidth - ICON_SIZE) / 2,
    motion.fastSpatialSpec(),
    label = "tab icon",
  )
  val labelAlpha by animateFloatAsState(if (isSelected && showsLabel) 1f else 0f, motion.fastEffectsSpec(), label = "tab label")
  val label = stringResource(tabLabel(tab))
  val count = if (countsUnread) maxOf(badge.mentions, badge.unread) else badge.mentions
  val description = when {
    count > 0 -> pluralStringResource(R.plurals.chat_tab_unread, count, label, count)
    badge.unread > 0 -> stringResource(R.string.chat_tab_new, label)
    else -> label
  }
  Row(
    modifier = Modifier
      .width(width)
      .height(ITEM_SIZE)
      .clip(CircleShape)
      .background(container)
      .selectable(selected = isSelected, onClick = onClick, role = Role.Tab, indication = ripple(), interactionSource = null)
      .semantics(mergeDescendants = true) { contentDescription = description }
      .padding(start = iconStart),
    verticalAlignment = Alignment.CenterVertically,
  ) {
    BadgedBox(
      badge = {
        when {
          count > 0 -> Badge(
            containerColor = LocalTeamCraftColors.current.badge,
            contentColor = LocalTeamCraftColors.current.chromeText,
          ) { Text(count.toString()) }
          badge.unread > 0 -> Badge(containerColor = MaterialTheme.colorScheme.outline)
        }
      },
    ) { Icon(tabIcon(tab), contentDescription = null, tint = content, modifier = Modifier.size(ICON_SIZE)) }
    Spacer(Modifier.width(ICON_LABEL_GAP))
    Text(
      label,
      style = MaterialTheme.typography.labelLarge,
      color = content,
      maxLines = 1,
      softWrap = false,
      overflow = TextOverflow.Clip,
      modifier = Modifier.graphicsLayer { alpha = labelAlpha },
    )
  }
}

private fun tabLabel(tab: ChatTab): Int = when (tab) {
  ChatTab.Home -> R.string.nav_home
  ChatTab.Direct -> R.string.nav_direct
  ChatTab.Activity -> R.string.nav_activity
}

private fun tabIcon(tab: ChatTab) = when (tab) {
  ChatTab.Home -> Icons.Filled.Home
  // The marker of DMs elsewhere in the app (their empty state).
  ChatTab.Direct -> Icons.Filled.Email
  ChatTab.Activity -> Icons.Filled.Notifications
}
