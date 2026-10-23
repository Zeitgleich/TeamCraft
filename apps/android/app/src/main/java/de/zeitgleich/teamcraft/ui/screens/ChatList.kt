package de.zeitgleich.teamcraft.ui.screens

import android.content.Context
import android.text.format.DateUtils
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Done
import androidx.compose.material.icons.filled.Email
import androidx.compose.material.icons.filled.Search
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material.icons.filled.Star
import androidx.compose.material3.AssistChip
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SwipeToDismissBox
import androidx.compose.material3.SwipeToDismissBoxValue
import androidx.compose.material3.Text
import androidx.compose.material3.rememberSwipeToDismissBoxState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.CustomAccessibilityAction
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.customActions
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.launch
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.ChatActions
import de.zeitgleich.teamcraft.ui.ChatListEntry
import de.zeitgleich.teamcraft.ui.ChatPreview
import de.zeitgleich.teamcraft.ui.MainViewModel
import de.zeitgleich.teamcraft.ui.PreviewContent
import de.zeitgleich.teamcraft.ui.PreviewSender
import de.zeitgleich.teamcraft.ui.SectionTitle
import de.zeitgleich.teamcraft.ui.sections
import de.zeitgleich.teamcraft.ui.theme.LocalTeamCraftColors
import uniffi.teamcraft_ffi.ConversationItem
import uniffi.teamcraft_ffi.ConversationKind

@Composable
fun ChatListScreen(
  viewModel: MainViewModel,
  onOpen: (String) -> Unit,
  onSearch: () -> Unit,
  onSettings: () -> Unit,
  onNavigate: (String) -> Unit,
  maybeSelectedId: String? = null,
) {
  var conversations by remember { mutableStateOf<List<ConversationItem>?>(null) }
  var previews by remember { mutableStateOf<Map<String, ChatListEntry>>(emptyMap()) }
  LaunchedEffect(Unit) {
    // Every read finishes before the next starts: while syncing, changes come faster than a full
    // pass over all conversations, and restarting on each would never show the previews.
    viewModel.changeCount.collect {
      val loaded = viewModel.conversations()
      conversations = loaded
      previews = viewModel.chatPreviews(loaded)
    }
  }
  Scaffold(
    topBar = {
      ChromeTopBar(
        title = stringResource(R.string.chats_title),
        actions = {
          IconButton(onClick = onSearch) {
            Icon(Icons.Filled.Search, contentDescription = stringResource(R.string.search))
          }
          IconButton(onClick = onSettings) {
            Icon(Icons.Filled.Settings, contentDescription = stringResource(R.string.settings_title))
          }
        },
      )
    },
  ) { padding ->
    Column(Modifier.fillMaxSize().padding(padding)) {
      OfflineBanner()
      LazyRow(
        Modifier.fillMaxWidth().padding(horizontal = 8.dp, vertical = 4.dp),
        horizontalArrangement = Arrangement.spacedBy(8.dp),
      ) {
        for ((route, label) in QUICK_ROUTES) {
          item(key = route) {
            AssistChip(onClick = { onNavigate(route) }, label = { Text(stringResource(label)) })
          }
        }
      }
      val loaded = conversations
      when {
        loaded == null -> LoadingScreen()
        loaded.isEmpty() -> EmptyChats(onBrowse = { onNavigate("browse") })
        else -> LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(bottom = 16.dp)) {
          val lastActivity = previews.mapValues { it.value.preview.sentAt }
          for (section in sections(loaded, lastActivity)) {
            item(key = "section-${section.title}") { SectionHeader(section.title) }
            items(section.conversations, key = { "${section.title}-${it.id}" }) { conversation ->
              SwipeableConversationRow(
                viewModel = viewModel,
                conversation = conversation,
                maybeEntry = previews[conversation.id],
                isSelected = conversation.id == maybeSelectedId,
                onClick = { onOpen(conversation.id) },
              )
            }
          }
        }
      }
    }
  }
}

@Composable
private fun EmptyChats(onBrowse: () -> Unit) {
  Column(
    Modifier.fillMaxSize().padding(32.dp),
    verticalArrangement = Arrangement.Center,
    horizontalAlignment = Alignment.CenterHorizontally,
  ) {
    Text(stringResource(R.string.no_conversations), style = MaterialTheme.typography.titleMedium)
    Text(
      stringResource(R.string.chats_empty_hint),
      style = MaterialTheme.typography.bodyMedium,
      color = MaterialTheme.colorScheme.onSurfaceVariant,
      modifier = Modifier.padding(top = 8.dp, bottom = 16.dp),
    )
    AssistChip(onClick = onBrowse, label = { Text(stringResource(R.string.browse_title)) })
  }
}

@Composable
private fun SectionHeader(title: SectionTitle) {
  val text = when (title) {
    SectionTitle.Favorites -> stringResource(R.string.favorites)
    SectionTitle.Channels -> stringResource(R.string.channels)
    SectionTitle.DirectMessages -> stringResource(R.string.direct_messages)
    is SectionTitle.Space -> title.name
  }
  Text(
    text.uppercase(),
    style = MaterialTheme.typography.labelMedium,
    color = MaterialTheme.colorScheme.onSurfaceVariant,
    modifier = Modifier.padding(start = 16.dp, top = 16.dp, bottom = 4.dp),
  )
}

/**
 * Swipe right toggles read/unread, swipe left toggles the favorite; both snap back. The same
 * actions are in the long-press menu and in the accessibility actions.
 */
@Composable
private fun SwipeableConversationRow(
  viewModel: MainViewModel,
  conversation: ConversationItem,
  maybeEntry: ChatListEntry?,
  isSelected: Boolean,
  onClick: () -> Unit,
) {
  val haptics = LocalHapticFeedback.current
  val scope = rememberCoroutineScope()
  val actions = rowActions(viewModel, conversation, maybeEntry)
  var isNotifying by remember { mutableStateOf(false) }
  val state = rememberSwipeToDismissBoxState()
  SwipeToDismissBox(
    state = state,
    enableDismissFromStartToEnd = actions.toggleRead != null,
    backgroundContent = { SwipeBackground(state.dismissDirection, conversation, isUnread = actions.isUnread) },
    onDismiss = { direction ->
      when (direction) {
        SwipeToDismissBoxValue.StartToEnd -> actions.toggleRead?.invoke()
        SwipeToDismissBoxValue.EndToStart -> actions.toggleFavorite()
        SwipeToDismissBoxValue.Settled -> Unit
      }
      haptics.performHapticFeedback(HapticFeedbackType.LongPress)
      // Never removed: the row snaps back.
      scope.launch { state.reset() }
    },
  ) {
    ConversationRow(conversation, maybeEntry, actions, isSelected, onClick, onNotifications = { isNotifying = true })
  }
  // Mute or change notifications without opening the conversation.
  if (isNotifying) ConversationNotificationsDialog(viewModel, conversation.id) { isNotifying = false }
}

/** What can be done with a row, shared by swipe, menu and accessibility. */
private class RowActions(
  val isUnread: Boolean,
  val toggleRead: (() -> Unit)?,
  val toggleFavorite: () -> Unit,
)

private fun rowActions(viewModel: MainViewModel, conversation: ConversationItem, maybeEntry: ChatListEntry?): RowActions {
  val isUnread = conversation.unreadCount > 0u
  val toggleRead: (() -> Unit)? = when {
    isUnread -> ({ viewModel.markRead(conversation.id) })
    maybeEntry != null -> ({ viewModel.act(ChatActions.markUnread(conversation.id, maybeEntry.lastMessageId)) })
    else -> null
  }
  return RowActions(
    isUnread = isUnread,
    toggleRead = toggleRead,
    toggleFavorite = { viewModel.act(ChatActions.setFavorite(conversation.id, !conversation.isFavorite)) },
  )
}

@Composable
private fun SwipeBackground(direction: SwipeToDismissBoxValue, conversation: ConversationItem, isUnread: Boolean) {
  val (color, icon, label, alignment) = when (direction) {
    SwipeToDismissBoxValue.StartToEnd -> SwipeLook(
      MaterialTheme.colorScheme.primary,
      if (isUnread) Icons.Filled.Done else Icons.Filled.Email,
      if (isUnread) R.string.chat_mark_read else R.string.chat_mark_unread,
      Alignment.CenterStart,
    )
    SwipeToDismissBoxValue.EndToStart -> SwipeLook(
      MaterialTheme.colorScheme.tertiary,
      Icons.Filled.Star,
      if (conversation.isFavorite) R.string.favorite_remove else R.string.favorite_add,
      Alignment.CenterEnd,
    )
    SwipeToDismissBoxValue.Settled -> return
  }
  Box(Modifier.fillMaxSize().background(color).padding(horizontal = 24.dp), contentAlignment = alignment) {
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
      Icon(icon, contentDescription = null, tint = MaterialTheme.colorScheme.onPrimary)
      Text(stringResource(label), color = MaterialTheme.colorScheme.onPrimary, style = MaterialTheme.typography.labelLarge)
    }
  }
}

private data class SwipeLook(
  val color: Color,
  val icon: ImageVector,
  val label: Int,
  val alignment: Alignment,
)

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun ConversationRow(
  conversation: ConversationItem,
  maybeEntry: ChatListEntry?,
  actions: RowActions,
  isSelected: Boolean,
  onClick: () -> Unit,
  onNotifications: () -> Unit,
) {
  val haptics = LocalHapticFeedback.current
  var isMenuOpen by remember { mutableStateOf(false) }
  val readLabel = stringResource(if (actions.isUnread) R.string.chat_mark_read else R.string.chat_mark_unread)
  val favoriteLabel = stringResource(if (conversation.isFavorite) R.string.favorite_remove else R.string.favorite_add)
  Box {
    Row(
      modifier = Modifier
        .fillMaxWidth()
        // The open conversation, when it shows next to the list (two panes).
        .background(if (isSelected) MaterialTheme.colorScheme.primaryContainer else MaterialTheme.colorScheme.background)
        .combinedClickable(
          onClick = onClick,
          onLongClick = {
            haptics.performHapticFeedback(HapticFeedbackType.LongPress)
            isMenuOpen = true
          },
        )
        .semantics {
          customActions = listOfNotNull(
            actions.toggleRead?.let { toggle -> CustomAccessibilityAction(readLabel) { toggle(); true } },
            CustomAccessibilityAction(favoriteLabel) { actions.toggleFavorite(); true },
          )
        }
        .padding(horizontal = 16.dp, vertical = 10.dp),
      verticalAlignment = Alignment.CenterVertically,
    ) {
      ConversationAvatar(conversation)
      Column(Modifier.weight(1f).padding(start = 12.dp)) {
        TitleLine(conversation, maybeEntry)
        PreviewLine(conversation, maybeEntry)
      }
    }
    DropdownMenu(expanded = isMenuOpen, onDismissRequest = { isMenuOpen = false }) {
      actions.toggleRead?.let { toggle ->
        DropdownMenuItem(text = { Text(readLabel) }, onClick = {
          isMenuOpen = false
          toggle()
        })
      }
      DropdownMenuItem(text = { Text(favoriteLabel) }, onClick = {
        isMenuOpen = false
        actions.toggleFavorite()
      })
      DropdownMenuItem(text = { Text(stringResource(R.string.notifications_conversation)) }, onClick = {
        isMenuOpen = false
        onNotifications()
      })
    }
  }
}

@Composable
private fun TitleLine(conversation: ConversationItem, maybeEntry: ChatListEntry?) {
  val context = LocalContext.current
  val isUnread = conversation.unreadCount > 0u
  Row(verticalAlignment = Alignment.CenterVertically) {
    Text(
      conversation.name,
      modifier = Modifier.weight(1f),
      maxLines = 1,
      overflow = TextOverflow.Ellipsis,
      style = MaterialTheme.typography.titleSmall,
      fontWeight = if (isUnread) FontWeight.Bold else FontWeight.Normal,
      color = if (conversation.isMuted) MaterialTheme.colorScheme.onSurfaceVariant else MaterialTheme.colorScheme.onSurface,
    )
    if (conversation.isMuted) {
      val mutedLabel = stringResource(R.string.chat_muted)
      Text(
        "🔕",
        style = MaterialTheme.typography.labelSmall,
        modifier = Modifier.padding(start = 4.dp).semantics { contentDescription = mutedLabel },
      )
    }
    maybeEntry?.let { entry ->
      Text(
        listTime(context, entry.preview.sentAt),
        style = MaterialTheme.typography.labelSmall,
        color = if (isUnread) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.padding(start = 8.dp),
      )
    }
  }
}

@Composable
private fun PreviewLine(conversation: ConversationItem, maybeEntry: ChatListEntry?) {
  val isUnread = conversation.unreadCount > 0u
  Row(verticalAlignment = Alignment.CenterVertically) {
    Text(
      maybeEntry?.let { previewText(it.preview) } ?: conversation.topic,
      modifier = Modifier.weight(1f),
      maxLines = 1,
      overflow = TextOverflow.Ellipsis,
      style = MaterialTheme.typography.bodyMedium,
      color = if (isUnread) MaterialTheme.colorScheme.onSurface else MaterialTheme.colorScheme.onSurfaceVariant,
    )
    if (conversation.mentionCount > 0u) {
      val count = conversation.mentionCount.toInt()
      Badge(
        "@${count}",
        LocalTeamCraftColors.current.badge,
        pluralStringResource(R.plurals.chat_mentions, count, count),
      )
    } else if (isUnread) {
      val count = conversation.unreadCount.toInt()
      Badge(
        count.toString(),
        if (conversation.isMuted) MaterialTheme.colorScheme.onSurfaceVariant else MaterialTheme.colorScheme.primary,
        pluralStringResource(R.plurals.chat_unread, count, count),
      )
    }
  }
}

@Composable
private fun previewText(preview: ChatPreview): String {
  val body = when (val content = preview.content) {
    is PreviewContent.Text -> content.text
    PreviewContent.Deleted -> stringResource(R.string.chat_preview_deleted)
    PreviewContent.Event -> stringResource(R.string.chat_preview_event)
  }
  return when (val sender = preview.sender) {
    PreviewSender.None -> body
    PreviewSender.Me -> stringResource(R.string.chat_preview_sender, stringResource(R.string.you), body)
    is PreviewSender.Other -> stringResource(R.string.chat_preview_sender, sender.name, body)
  }
}

/** Today: the time; this year: day and month; older: the full date. */
private fun listTime(context: Context, sentAt: Long): String {
  val flags = when {
    DateUtils.isToday(sentAt) -> DateUtils.FORMAT_SHOW_TIME
    System.currentTimeMillis() - sentAt < DateUtils.WEEK_IN_MILLIS -> DateUtils.FORMAT_SHOW_WEEKDAY or DateUtils.FORMAT_ABBREV_WEEKDAY
    else -> DateUtils.FORMAT_SHOW_DATE or DateUtils.FORMAT_ABBREV_MONTH
  }
  return DateUtils.formatDateTime(context, sentAt, flags)
}

/** A round marker with the conversation's initial (people) or its kind (channels). */
@Composable
private fun ConversationAvatar(conversation: ConversationItem) {
  val isPerson = conversation.kind == ConversationKind.DIRECT || conversation.kind == ConversationKind.GROUP
  val label = if (isPerson) conversation.name.firstOrNull()?.uppercase() ?: "?" else prefix(conversation.kind)
  Box(
    Modifier
      .size(40.dp)
      .background(
        if (isPerson) avatarColor(conversation.name) else MaterialTheme.colorScheme.surfaceVariant,
        CircleShape,
      ),
    contentAlignment = Alignment.Center,
  ) {
    Text(
      label,
      style = MaterialTheme.typography.titleMedium,
      color = if (isPerson) Color.White else MaterialTheme.colorScheme.onSurfaceVariant,
    )
  }
}

/** A stable hue per name, so people are recognizable at a glance. */
private fun avatarColor(name: String): Color = Color.hsl(((name.hashCode() % 360 + 360) % 360).toFloat(), 0.45f, 0.45f)

@Composable
private fun Badge(text: String, color: Color, description: String) {
  Text(
    text,
    color = LocalTeamCraftColors.current.chromeText,
    style = MaterialTheme.typography.labelSmall,
    modifier = Modifier
      .padding(start = 8.dp)
      .background(color, RoundedCornerShape(10.dp))
      .padding(horizontal = 7.dp, vertical = 2.dp)
      .semantics { contentDescription = description },
  )
}

/** The sidebar's markers: `#` public, lock-like `◆` private, `@` direct. */
private fun prefix(kind: ConversationKind): String = when (kind) {
  ConversationKind.PUBLIC -> "#"
  ConversationKind.PRIVATE -> "◆"
  ConversationKind.DIRECT -> "@"
  ConversationKind.GROUP -> "@"
  ConversationKind.INTEGRATION -> "⚙"
}

/** Entry points above the conversation list. */
private val QUICK_ROUTES = listOf(
  "threads" to R.string.threads_title,
  "saved" to R.string.saved_title,
  "browse" to R.string.browse_title,
  "meetings" to R.string.meetings_title,
)
