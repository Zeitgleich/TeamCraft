package de.zeitgleich.teamcraft.ui.forms

import androidx.annotation.StringRes
import de.zeitgleich.teamcraft.R

/** Longest personal note the server takes with an invitation (`max_text_length`, default 1000). */
const val MAX_INVITE_NOTE_CHARS = 1000

/** The fields of "Invite people" (#1337), top to bottom. */
enum class InviteField { EMAIL, NOTE }

/**
 * The checks of an invitation, before the server has its say (it also checks the address and
 * which domains guests may come from): an address with something on both sides of one `@`, a note
 * the server takes.
 */
fun inviteErrors(email: String, note: String): FieldErrors<InviteField> = FieldErrors.of(
  InviteField.EMAIL to emailError(email),
  InviteField.NOTE to if (note.trim().length > MAX_INVITE_NOTE_CHARS) R.string.create_description_too_long else null,
)

@StringRes
private fun emailError(email: String): Int? {
  val trimmed = email.trim()
  if (trimmed.isEmpty()) return R.string.invite_email_missing
  val parts = trimmed.split('@')
  val isAddress = parts.size == 2 && parts.all { it.isNotEmpty() } && '.' in parts[1] && trimmed.none(Char::isWhitespace)
  return if (isAddress) null else R.string.invite_email_invalid
}
