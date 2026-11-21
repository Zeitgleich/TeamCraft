package de.zeitgleich.teamcraft.ui

import androidx.navigation.NavBackStackEntry
import androidx.navigation.NavController
import androidx.navigation.NavOptions
import androidx.navigation.navOptions

/** The signed-in navigation's first screen: the conversation list. */
const val HOME_ROUTE = "chats"

/**
 * Where a request to open a screen comes from. From outside the app (a notification, a bubble, an
 * app link, an app shortcut, the widget, a share) the screen replaces whatever was open: Back leads
 * to the list, never to a chat that happened to be open before (#2240). Inside the app screens stack,
 * except chats ([chatScreensToPop]).
 */
enum class OpenOrigin {
  InApp,
  Outside,
}

/** A screen on the back stack that shows a chat: a conversation, or one of its threads. */
data class ChatScreen(val conversationId: String, val isThread: Boolean)

/** How a route goes on the back stack when opened from [origin]. */
fun openOptions(origin: OpenOrigin): NavOptions = navOptions {
  when (origin) {
    OpenOrigin.InApp -> Unit
    OpenOrigin.Outside -> {
      // The list stays, everything above it goes: the stack is [list, route].
      popUpTo(HOME_ROUTE)
      launchSingleTop = true
    }
  }
}

/**
 * How many screens to pop from the top of [stack] (bottom first, `null` for a screen that shows no
 * chat) before [target] opens, so chats never pile up (#2240). A conversation replaces every chat on
 * the stack and what was opened above it; a thread replaces the chats of other conversations and
 * stacks on its own conversation. Screens below the first chat (the list, search, saved) stay.
 */
fun chatScreensToPop(stack: List<ChatScreen?>, target: ChatScreen): Int {
  val first = stack.indexOfFirst { it != null && (!target.isThread || it.conversationId != target.conversationId) }
  return if (first < 0) 0 else stack.size - first
}

/** Opens [route] for an entry from outside the app: the stack becomes [list, route]. */
fun NavController.openFromOutside(route: String) {
  navigate(route, openOptions(OpenOrigin.Outside))
}

/** Closes every screen above the list, for an entry from outside that shows the list itself. */
fun NavController.showHomeFromOutside() {
  popBackStack(HOME_ROUTE, inclusive = false)
}

/** Opens a conversation from inside the app, in place of the chats on the stack. */
fun NavController.openConversation(conversationId: String, maybeFocus: String? = null) {
  val route = maybeFocus?.let { "conversation/$conversationId?focus=$it" } ?: "conversation/$conversationId"
  openChat(route, ChatScreen(conversationId, isThread = false))
}

/** Opens a thread from inside the app: on its conversation, in place of other conversations' chats. */
fun NavController.openThread(conversationId: String, threadId: String, maybeFocus: String? = null) {
  val route = "thread/$conversationId/$threadId" + (maybeFocus?.let { "?focus=$it" } ?: "")
  openChat(route, ChatScreen(conversationId, isThread = true))
}

private fun NavController.openChat(route: String, target: ChatScreen) {
  val stack = currentBackStack.value.map(::chatScreenOf)
  repeat(chatScreensToPop(stack, target)) { popBackStack() }
  navigate(route)
}

private fun chatScreenOf(entry: NavBackStackEntry): ChatScreen? {
  val route = entry.destination.route ?: return null
  val arguments = entry.arguments ?: return null
  return when {
    route.startsWith("conversation/") -> arguments.getString("id")?.let { ChatScreen(it, isThread = false) }
    route.startsWith("thread/") -> arguments.getString("conversation")?.let { ChatScreen(it, isThread = true) }
    else -> null
  }
}

/** Opens a conversation, or a thread of it, from inside the app; at [maybeFocus] when given. */
fun NavController.openChatAt(conversationId: String, maybeThreadId: String?, maybeFocus: String?) {
  when (maybeThreadId) {
    null -> openConversation(conversationId, maybeFocus)
    else -> openThread(conversationId, maybeThreadId, maybeFocus)
  }
}
