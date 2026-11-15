package de.zeitgleich.teamcraft.ui

import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.draganddrop.DragAndDropEvent
import androidx.compose.ui.input.key.Key
import androidx.compose.ui.input.key.KeyEvent
import androidx.compose.ui.input.key.KeyEventType
import androidx.compose.ui.input.key.isAltPressed
import androidx.compose.ui.input.key.isCtrlPressed
import androidx.compose.ui.input.key.isShiftPressed
import androidx.compose.ui.input.key.key
import androidx.compose.ui.input.key.type
import kotlinx.coroutines.flow.MutableSharedFlow

/** What a hardware keyboard asks for (#2075); the screen that can do it takes the request. */
enum class ShortcutAction {
  /** Ctrl+K or Ctrl+F: the search, which is also the quick switcher. */
  Search,

  /** Alt+Shift+Down: the next conversation with unread messages. */
  NextUnread,

  /** Esc: back, out of a conversation or a settings screen. */
  Close,
}

/** The shortcut for [event], or null: the app-wide ones, handled where the navigation is. */
fun shortcutFor(event: KeyEvent): ShortcutAction? {
  if (event.type != KeyEventType.KeyDown) return null
  return when {
    event.isCtrlPressed && (event.key == Key.K || event.key == Key.F) -> ShortcutAction.Search
    event.isAltPressed && event.isShiftPressed && event.key == Key.DirectionDown -> ShortcutAction.NextUnread
    event.key == Key.Escape && !event.isCtrlPressed && !event.isAltPressed -> ShortcutAction.Close
    else -> null
  }
}

/** Ctrl+Enter sends whatever the Enter setting says; Enter alone sends when the setting is on. */
fun isSendShortcut(event: KeyEvent, isSendWithEnter: Boolean): Boolean {
  if (event.type != KeyEventType.KeyDown || event.key != Key.Enter) return false
  return if (event.isCtrlPressed) true else isSendWithEnter && !event.isShiftPressed
}

/** Up in an empty composer edits the user's last message. */
fun isEditLastShortcut(event: KeyEvent, isTextEmpty: Boolean): Boolean =
  isTextEmpty && event.type == KeyEventType.KeyDown && event.key == Key.DirectionUp && !event.isShiftPressed && !event.isAltPressed

/** Requests that cross screens: the composer asks, the message list answers. */
object ChatShortcuts {
  val editLast = MutableSharedFlow<Unit>(extraBufferCapacity = 1)
}

/**
 * The conversation after [currentId] in list order that has unread messages, wrapping around; null
 * when no other one has any.
 */
fun nextUnreadAfter(order: List<Pair<String, Int>>, currentId: String?): String? {
  if (order.isEmpty()) return null
  val start = order.indexOfFirst { it.first == currentId }
  val rotated = if (start < 0) order else order.drop(start + 1) + order.take(start)
  return rotated.firstOrNull { (id, unread) -> unread > 0 && id != currentId }?.first
}

/**
 * Files dragged over a conversation (#2074): tablets, split screen and ChromeOS. The scaffold draws
 * the highlight from [isOver]; the composer, which holds the picked files, sets [maybeHandler].
 */
@Stable
class ChatDrop {
  var isOver by mutableStateOf(false)
  var maybeHandler: ((DragAndDropEvent) -> Boolean)? = null
}
