package de.zeitgleich.teamcraft.ui

import de.zeitgleich.teamcraft.core.Message
import de.zeitgleich.teamcraft.core.MessageExtras

// Selecting several messages of one conversation (issue #2070): the rules behind the selection
// bar, kept free of Compose so forward, copy and delete act on the same ordered list. The same
// rules as the web client's `selection.rs`.

/** Adds the message to the selection, or takes it out when it is in; the mode stays when it empties. */
internal fun Set<String>.toggled(id: String): Set<String> = if (id in this) this - id else this + id

/** The selection without the messages deleted or gone meanwhile (the feed no longer has them). */
internal fun Set<String>.surviving(messages: List<Message>, extras: Map<String, MessageExtras>): Set<String> {
  val present = messages.asSequence().filter { extras[it.id]?.isDeleted != true }.map { it.id }.toSet()
  return filterTo(HashSet()) { it in present }
}

/** The selected user messages, oldest first: the order they are forwarded and copied in. */
internal fun selectedInOrder(messages: List<Message>, selection: Set<String>): List<Message> =
  messages
    .filter { it.id in selection && it.maybePlaceholder == null }
    .sortedWith(compareBy<Message> { it.sentAt }.thenBy { it.id.toLongOrNull() ?: 0L })

/**
 * Whether the user may delete every selected message: only their own, imported ones excepted, and
 * only where posting is on (the rule of a single message's Delete in [messageActions]).
 */
internal fun canDeleteAll(selected: List<Message>, myId: String?, canPost: Boolean): Boolean =
  canPost && myId != null && selected.isNotEmpty() && selected.all { it.author == myId && it.maybeImported == null }

/** The text of Copy: per message the sender and time on a line, then the text; blank line between. */
internal fun copiedText(selected: List<Message>, nameOf: (String) -> String, timeOf: (Long) -> String): String =
  selected.joinToString("\n\n") { "${nameOf(it.author)}, ${timeOf(it.sentAt)}\n${it.body}" }
