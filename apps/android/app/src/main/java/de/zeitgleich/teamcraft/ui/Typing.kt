package de.zeitgleich.teamcraft.ui

import androidx.compose.runtime.Composable
import androidx.compose.ui.res.stringResource
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.TypingPlace

/** How often the app tells the core that the user types; the core and the server throttle too. */
const val TYPING_NOTIFY_INTERVAL_MS = 3_000L

/** How often typing is read again while someone types (indicators last 6 s and run out without an event). */
const val TYPING_POLL_MS = 2_000L

/** The "… is typing" line under a conversation, by how many people type. */
sealed interface TypingLine {
  data class One(val name: String) : TypingLine
  data class Two(val first: String, val second: String) : TypingLine
  data object Several : TypingLine
}

/** The line for these names, or nothing when nobody types. */
fun typingLine(names: List<String>): TypingLine? = when (names.size) {
  0 -> null
  1 -> TypingLine.One(names[0])
  2 -> TypingLine.Two(names[0], names[1])
  else -> TypingLine.Several
}

/** Whether a change of the composer text should announce typing now. */
fun shouldNotifyTyping(text: String, nowMs: Long, lastNotifiedMs: Long?): Boolean =
  text.isNotBlank() && (lastNotifiedMs == null || nowMs - lastNotifiedMs >= TYPING_NOTIFY_INTERVAL_MS)

/** Those typing in this place: the conversation itself, or with [maybeThread] a reply in that thread. */
fun typingIn(places: List<TypingPlace>, conversation: String, maybeThread: String? = null): List<String> =
  places.firstOrNull { it.conversation == conversation && it.maybeThread == maybeThread }?.users.orEmpty()

/** Who types in each conversation itself (thread replies don't count), by [nameOf] their ID. */
fun typingNamesByConversation(places: List<TypingPlace>, nameOf: (String) -> String): Map<String, List<String>> =
  places
    .filter { it.maybeThread == null && it.users.isNotEmpty() }
    .associate { place -> place.conversation to place.users.map(nameOf) }

/**
 * The line's text. In a direct message the other person is the conversation, so a bare "typing…"
 * does ([isDirect]); everywhere else it names who types.
 */
@Composable
fun typingText(line: TypingLine, isDirect: Boolean = false): String = when {
  isDirect && line is TypingLine.One -> stringResource(R.string.typing_direct)
  line is TypingLine.One -> stringResource(R.string.typing_one, line.name)
  line is TypingLine.Two -> stringResource(R.string.typing_two, line.first, line.second)
  else -> stringResource(R.string.typing_several)
}
