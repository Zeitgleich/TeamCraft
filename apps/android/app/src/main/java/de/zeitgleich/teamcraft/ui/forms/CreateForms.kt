package de.zeitgleich.teamcraft.ui.forms

import androidx.annotation.StringRes
import de.zeitgleich.teamcraft.R

/** Longest channel or space name the server takes (`max_name_length`, default 100). */
const val MAX_NAME_CHARS = 100

/** Longest description the server takes (`max_text_length`, default 1000). */
const val MAX_DESCRIPTION_CHARS = 1000

/** How a create form's request ended (#1534). */
sealed interface CreateResult {
  /** Created or found: the new conversation's or space's ID. */
  data class Created(val id: String) : CreateResult

  /** The server refused the name: shown at the name field. */
  data object NameRefused : CreateResult

  /** Not created, for a reason that belongs to no field: shown next to the button. */
  data class Failed(@StringRes val text: Int) : CreateResult
}

/** The fields of "New channel" and "New space", top to bottom. */
enum class CreateField { NAME, DESCRIPTION }

/** The checks of a new channel's or space's name and description, the same as the server's. */
fun createErrors(name: String, description: String, @StringRes whenNameEmpty: Int): FieldErrors<CreateField> =
  FieldErrors.of(
    CreateField.NAME to when {
      name.isBlank() -> whenNameEmpty
      name.trim().length > MAX_NAME_CHARS -> R.string.create_name_too_long
      name.any(Char::isISOControl) -> R.string.create_name_invalid
      else -> null
    },
    CreateField.DESCRIPTION to if (description.trim().length > MAX_DESCRIPTION_CHARS) R.string.create_description_too_long else null,
  )
