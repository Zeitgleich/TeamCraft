package de.zeitgleich.teamcraft.ui.forms

import androidx.annotation.StringRes
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.ui.channelNameProblemText
import uniffi.teamcraft_ffi.ChannelNameProblem

/** Longest channel or space name the server takes (`max_name_length`, default 100). */
const val MAX_NAME_CHARS = 100

/** Longest description the server takes (`max_text_length`, default 1000). */
const val MAX_DESCRIPTION_CHARS = 1000

/** How a create form's request ended (#1534). */
sealed interface CreateResult {
  /** Created or found: the new conversation's or space's ID. */
  data class Created(val id: String) : CreateResult

  /** The name was refused (by the server, or the channel name rules): [text] shows at the name field. */
  data class NameRefused(@StringRes val text: Int) : CreateResult

  /** Not created, for a reason that belongs to no field: shown next to the button. */
  data class Failed(@StringRes val text: Int) : CreateResult
}

/** Why adding people stopped: [text], naming [maybeName] where it is about somebody. */
data class AddPeopleFailure(@StringRes val text: Int, val maybeName: String? = null)

/** The fields of "New channel" and "New space", top to bottom. */
enum class CreateField { NAME, DESCRIPTION }

/**
 * The checks of a new channel's or space's name and description, the same as the server's. A
 * channel's name also keeps the channel name rules (e2ee.md §4.6.5): [maybeNameProblem] is what the
 * core's check found (`checkChannelName`).
 */
fun createErrors(
  name: String,
  description: String,
  @StringRes whenNameEmpty: Int,
  maybeNameProblem: ChannelNameProblem? = null,
): FieldErrors<CreateField> =
  FieldErrors.of(
    CreateField.NAME to when {
      name.isBlank() -> whenNameEmpty
      name.trim().length > MAX_NAME_CHARS -> R.string.create_name_too_long
      maybeNameProblem != null && channelNameProblemText(maybeNameProblem) != null -> channelNameProblemText(maybeNameProblem)
      name.any(Char::isISOControl) -> R.string.create_name_invalid
      else -> null
    },
    CreateField.DESCRIPTION to if (description.trim().length > MAX_DESCRIPTION_CHARS) R.string.create_description_too_long else null,
  )
