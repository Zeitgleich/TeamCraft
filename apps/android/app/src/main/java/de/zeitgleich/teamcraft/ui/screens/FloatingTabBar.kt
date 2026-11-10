package de.zeitgleich.teamcraft.ui.screens

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.animateColorAsState
import androidx.compose.animation.core.animateDpAsState
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.expandHorizontally
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.shrinkHorizontally
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
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Edit
import androidx.compose.material.icons.filled.Email
import androidx.compose.material.icons.filled.Home
import androidx.compose.material.icons.filled.Notifications
import androidx.compose.material3.Badge
import androidx.compose.material3.BadgedBox
import androidx.compose.material3.ExperimentalMaterial3ExpressiveApi
import androidx.compose.material3.FloatingToolbarDefaults
import androidx.compose.material3.HorizontalFloatingToolbar
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.ripple
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
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
private val FAB_GAP = 8.dp
private val FAB_SIZE = 56.dp

/** The toolbar's content padding on both sides plus the screen margins around pill and button. */
private val TOOLBAR_CHROME = 16.dp + 32.dp

/** Space the floating bar covers above the navigation bar: lists end this far up so their last row stays clear. */
private val FLOATING_BAR_COVER = FloatingToolbarDefaults.ContainerSize + FloatingToolbarDefaults.ScreenOffset + 8.dp

/** The main screen's action next to the tabs: "New" on Home, a new message on DMs, none on Activity. */
enum class MainAction { New, NewMessage }

/**
 * Bottom room for the main screen's lists: the floating bar plus the navigation bar beneath it, so
 * the lists scroll behind both (edge to edge) and their last row ends clear of them.
 */
@Composable
fun floatingTabBarClearance(): Dp = FLOATING_BAR_COVER + WindowInsets.navigationBars.asPaddingValues().calculateBottomPadding()

/**
 * The main screen's navigation as Google's current apps show it with Material 3 Expressive (issue
 * #454): a floating toolbar with Home, DMs and Activity, the selected tab widened to icon and label
 * in a tonal pill, the others icon-only with their badges, and the main action as a floating button
 * beside it. Without an action the pill re-centres.
 */
@OptIn(ExperimentalMaterial3ExpressiveApi::class)
@Composable
fun FloatingTabBar(
  selected: ChatTab,
  isList: Boolean,
  conversations: List<ConversationItem>,
  joinRequestCount: Int,
  maybeAction: MainAction?,
  onSelect: (ChatTab) -> Unit,
  onAction: (MainAction) -> Unit,
  modifier: Modifier = Modifier,
) {
  val haptics = LocalHapticFeedback.current
  // The list layout has the DMs on Home and no tab of their own for them.
  val tabs = ChatTab.entries.filterNot { isList && it == ChatTab.Direct }
  val labelWidth = rememberSelectedItemWidth(tabs)
  // Narrow windows with a large font: the label would push the button off screen, so the selected
  // tab keeps its highlight but shows its icon only.
  val needed = labelWidth + (ITEM_SIZE + 4.dp) * (tabs.size - 1) + TOOLBAR_CHROME + FAB_GAP + FAB_SIZE
  val showsLabel = needed <= LocalWindowInfo.current.containerDpSize.width
  val selectedWidth = if (showsLabel) labelWidth else ITEM_SIZE
  val motion = MaterialTheme.motionScheme
  val (direct, channels) = conversations.partition { it.isDirect }
  val lastAction = remember { mutableStateOf(MainAction.New) }
  maybeAction?.let { lastAction.value = it }
  Row(modifier, verticalAlignment = Alignment.CenterVertically) {
    HorizontalFloatingToolbar(expanded = true) {
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
          TabItem(tab, badge, countsUnread, isSelected = tab == selected, selectedWidth, showsLabel) {
            if (tab != selected) haptics.performHapticFeedback(HapticFeedbackType.SegmentTick)
            onSelect(tab)
          }
        }
      }
    }
    AnimatedVisibility(
      visible = maybeAction != null,
      enter = expandHorizontally(motion.defaultSpatialSpec()) + fadeIn(motion.defaultEffectsSpec()),
      exit = shrinkHorizontally(motion.defaultSpatialSpec()) + fadeOut(motion.fastEffectsSpec()),
    ) {
      // The button keeps its last action while it leaves, so it doesn't flicker to another icon.
      val action = maybeAction ?: lastAction.value
      FloatingToolbarDefaults.VibrantFloatingActionButton(
        onClick = { onAction(action) },
        modifier = Modifier.padding(start = FAB_GAP).size(FAB_SIZE),
      ) {
        when (action) {
          MainAction.New -> Icon(Icons.Filled.Add, contentDescription = stringResource(R.string.create_new))
          MainAction.NewMessage -> Icon(Icons.Filled.Edit, contentDescription = stringResource(R.string.dm_new))
        }
      }
    }
  }
}

/**
 * The selected tab is as wide as the longest label in the current language: selected and
 * deselected tabs then animate with the same spring in opposite directions, so the pill keeps its
 * width and doesn't re-centre on every tap.
 */
@Composable
private fun rememberSelectedItemWidth(tabs: List<ChatTab>): Dp {
  val measurer = rememberTextMeasurer()
  val style = MaterialTheme.typography.labelLarge
  val labels = tabs.map { stringResource(tabLabel(it)) }
  val density = LocalDensity.current
  return remember(labels, style, density) {
    val widest = labels.maxOf { measurer.measure(it, style, maxLines = 1).size.width }
    with(density) { widest.toDp() } + ICON_SIZE + ICON_LABEL_GAP + ITEM_PADDING * 2
  }
}

@Composable
private fun TabItem(
  tab: ChatTab,
  badge: TabBadge,
  countsUnread: Boolean,
  isSelected: Boolean,
  selectedWidth: Dp,
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
  val width by animateDpAsState(if (isSelected) selectedWidth else ITEM_SIZE, motion.fastSpatialSpec(), label = "tab width")
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
      .padding(horizontal = ITEM_PADDING),
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
