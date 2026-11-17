package de.zeitgleich.teamcraft.widget

import android.content.Context
import android.util.Log
import androidx.glance.appwidget.GlanceAppWidgetManager
import androidx.glance.appwidget.updateAll
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.FlowPreview
import kotlinx.coroutines.channels.BufferOverflow
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.debounce
import kotlinx.coroutines.flow.drop
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.merge
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.ChatClient
import de.zeitgleich.teamcraft.core.CoreRepository
import de.zeitgleich.teamcraft.notifications.NotificationContentPrefs
import de.zeitgleich.teamcraft.security.AppLockPrefs
import de.zeitgleich.teamcraft.ui.PreviewContent
import de.zeitgleich.teamcraft.ui.PreviewSender
import de.zeitgleich.teamcraft.ui.activityConversations
import de.zeitgleich.teamcraft.ui.chatPreview
import de.zeitgleich.teamcraft.ui.listName
import uniffi.teamcraft_ffi.AccountState
import uniffi.teamcraft_ffi.ConversationItem
import uniffi.teamcraft_ffi.ConversationKind
import uniffi.teamcraft_ffi.NotificationContent
import uniffi.teamcraft_ffi.rowBadge

/**
 * Keeps the home screen widget (issue #2076) up to date from the core's own change events: a push
 * that wakes the app, the live sync while it is open, reading a conversation, a changed setting. No
 * timer and no request of its own: when nothing changes, nothing runs. Without a widget on the
 * home screen it does nothing at all.
 */
class HomeWidgets(
  private val context: Context,
  private val core: CoreRepository,
  private val contentPrefs: NotificationContentPrefs,
  private val store: WidgetSnapshotStore,
  private val scope: CoroutineScope,
) {
  private val prefs = context.getSharedPreferences(PREFS_FILE, Context.MODE_PRIVATE)
  private val previewWanted = MutableStateFlow(prefs.getBoolean(KEY_PREVIEWS, false))
  private val requests = MutableSharedFlow<Unit>(extraBufferCapacity = 1, onBufferOverflow = BufferOverflow.DROP_OLDEST)
  private val writeLock = Mutex()
  private var maybeShown: WidgetSnapshot? = null

  /** The widget's own opt-in to message previews; off by default. */
  val isPreviewWanted: StateFlow<Boolean> = previewWanted

  @OptIn(FlowPreview::class)
  fun start() {
    scope.launch {
      merge(
        // The first value is the store opening, not a change; the next ones are.
        core.changeCount.drop(1).map {},
        contentPrefs.content.drop(1).map {},
        previewWanted.drop(1).map {},
        requests,
      ).debounce(DEBOUNCE_MS).collect { runCatching { refresh() }.onFailure(::reportFailure) }
    }
  }

  fun setPreviewWanted(isWanted: Boolean) {
    prefs.edit().putBoolean(KEY_PREVIEWS, isWanted).apply()
    previewWanted.value = isWanted
  }

  /** Reads again soon: a setting without its own flow changed (the app lock), or a widget was placed. */
  fun requestRefresh() {
    requests.tryEmit(Unit)
  }

  /** Whether a snapshot was ever written for the signed-in account. */
  fun hasSnapshot(): Boolean = store.exists()

  /** Signed out: the widget forgets the account at once. */
  fun clear() {
    scope.launch(Dispatchers.IO) {
      writeLock.withLock {
        store.delete()
        maybeShown = null
      }
      if (isPlaced()) runCatching { UnreadWidget().updateAll(context) }.onFailure(::reportFailure)
    }
  }

  private suspend fun refresh() {
    if (!isPlaced()) return
    val privacy = privacy() ?: return
    val snapshot = if (privacy == WidgetPrivacy.LOCKED) {
      // Not even the store is read: the lock hides everything.
      widgetSnapshot(privacy, emptyList())
    } else {
      // Signed out or the store not readable: what is shown stays (sign-out clears it).
      val conversations = runCatching { core.conversations() }.getOrNull() ?: return
      // widgetSnapshot drops the previews unless they may show.
      widgetSnapshot(privacy, rows(conversations, isReadingNewest = privacy != WidgetPrivacy.COUNT_ONLY))
    }
    val isChanged = writeLock.withLock {
      if (snapshot == maybeShown) {
        false
      } else {
        withContext(Dispatchers.IO) { store.write(snapshot) }
        maybeShown = snapshot
        true
      }
    }
    if (isChanged) UnreadWidget().updateAll(context)
  }

  /**
   * The app lock first (nothing is read then); otherwise the stricter of this device's notification
   * setting and the workspace policy, as the notifications themselves. `null` while the policy
   * can't be read (signed out, store not open): what is shown stays.
   */
  private suspend fun privacy(): WidgetPrivacy? {
    if (AppLockPrefs.prefs(context).isEnabled) return WidgetPrivacy.LOCKED
    val workspace = runCatching { core.core().notificationContent().workspace }.getOrNull() ?: return null
    return widgetPrivacy(stricter(contentPrefs.content.value, workspace), isAppLockOn = false, isPreviewWanted = previewWanted.value)
  }

  /**
   * The unread conversations in the chat list's "Unread" order (mentions, then the newest message),
   * named as the list names them. With [isReadingNewest] the newest messages are read for the
   * order and the preview line; [widgetSnapshot] keeps the line only where previews may show.
   */
  private suspend fun rows(conversations: List<ConversationItem>, isReadingNewest: Boolean): List<WidgetRow> {
    val unread = activityConversations(conversations)
    if (unread.isEmpty() || !isReadingNewest) return unread.map { row(it, maybePreview = null) }
    val wanted = unread.take(WIDGET_MAX_ROWS)
    val chat = ChatClient(core = { core.core() })
    val maybeRead = runCatching { chat.chatPreviews(wanted.map { it.id }) }.getOrNull()
    val maybeMyId = runCatching { (core.core().accountState() as? AccountState.Ready)?.userId }.getOrNull()
    val authors = maybeRead?.authors.orEmpty().associateBy { it.id }
    val deletedName = context.getString(R.string.user_deleted_name)
    val newest = maybeRead?.messages.orEmpty().associateBy { it.conversation }
    val lastActivity = newest.mapValues { (_, message) -> message.sentAt }
    return activityConversations(wanted, lastActivity).map { conversation ->
      val maybeText = newest[conversation.id]?.let { message ->
        val preview = chatPreview(message, maybeMyId.orEmpty(), message.authorName(authors, deletedName), conversation.isOneToOne())
        previewLine(preview.sender, preview.content)
      }
      row(conversation, maybeText)
    } + unread.drop(WIDGET_MAX_ROWS).map { row(it, maybePreview = null) }
  }

  private fun row(conversation: ConversationItem, maybePreview: String?): WidgetRow = WidgetRow(
    conversationId = conversation.id,
    name = conversation.listName(context.getString(R.string.notes_title)),
    countLabel = rowBadge(conversation.unreadCount, conversation.mentionCount, conversation.isMuted)?.label.orEmpty(),
    maybePreview = maybePreview,
  )

  /** The chat list's second line; events, deleted and unreadable messages show none here. */
  private fun previewLine(sender: PreviewSender, content: PreviewContent): String? {
    val text = (content as? PreviewContent.Text)?.text ?: return null
    return when (sender) {
      PreviewSender.None -> text
      PreviewSender.Me -> context.getString(R.string.chat_preview_sender, context.getString(R.string.you), text)
      is PreviewSender.Other -> context.getString(R.string.chat_preview_sender, sender.name, text)
    }
  }

  private suspend fun isPlaced(): Boolean =
    runCatching { GlanceAppWidgetManager(context).getGlanceIds(UnreadWidget::class.java).isNotEmpty() }.getOrDefault(false)

  private fun reportFailure(failure: Throwable) {
    // Names and text never go to the log; the type is enough to find it.
    Log.w(TAG, "widget not updated: ${failure.javaClass.simpleName}")
  }

  private companion object {
    const val TAG = "HomeWidgets"
    const val PREFS_FILE = "home_widget"
    const val KEY_PREVIEWS = "previews"
    /** Changes come in bursts while syncing; one update after they settle. */
    const val DEBOUNCE_MS = 1_000L
  }
}

/** The one that shows less; the enum goes from most to least shown. */
private fun stricter(device: NotificationContent, workspace: NotificationContent): NotificationContent =
  if (workspace.ordinal > device.ordinal) workspace else device

private fun ConversationItem.isOneToOne(): Boolean = kind == ConversationKind.DIRECT
