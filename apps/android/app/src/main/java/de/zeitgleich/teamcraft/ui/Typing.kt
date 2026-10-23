package de.zeitgleich.teamcraft.ui

/** How often the app tells the core that the user types; the core and the server throttle too. */
const val TYPING_NOTIFY_INTERVAL_MS = 3_000L

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
