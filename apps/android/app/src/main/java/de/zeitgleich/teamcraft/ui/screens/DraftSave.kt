package de.zeitgleich.teamcraft.ui.screens

/**
 * Whether the composer's text waits for the typing pause before it is stored as the draft. Each
 * edit restarts the pause; the wait of an earlier edit then stores nothing. Leaving (another
 * conversation, the thread closes, the app goes to the background) stores a waiting text at once,
 * or the last words typed are lost (#1220). Text that is already stored is not written again.
 */
internal class DraftSave {
  private var edits = 0
  private var maybePending: String? = null
  private var maybeStored: String? = null

  /** The draft was read from the core, or the composer stored [text] itself (sent, cleared). */
  fun stored(text: String) {
    maybePending = null
    maybeStored = text
  }

  /** The text changed: returns what the pause's wait hands to [takeDue]. */
  fun edit(text: String): Int {
    edits += 1
    maybePending = text.takeIf { it != maybeStored }
    return edits
  }

  /** The pause after [edit] is over: the text to store now, unless a later edit restarted it. */
  fun takeDue(edit: Int): String? = if (edit == edits) takePending() else null

  /** The composer or the app goes: the text to store now, if one waits. */
  fun takePending(): String? {
    val text = maybePending ?: return null
    stored(text)
    return text
  }
}
