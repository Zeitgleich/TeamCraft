package de.zeitgleich.teamcraft.ui.forms

import androidx.annotation.StringRes
import de.zeitgleich.teamcraft.R

/** Shortest recovery passphrase: the enrollment, the admins' recovery and a change use the same floor. */
const val MIN_PASSPHRASE_LENGTH = 12

/** Digits of the code a new device shows while it is linked (rendezvous short code). */
const val LINK_CODE_LENGTH = 6

/**
 * The errors of a form, one per field at most (forms.md, "Errors at the field"). [F] lists the
 * fields in the order they appear, so [maybeFirst] is the field a refused submit focuses. No
 * Compose and no Android here: unit tests cover every form's rules.
 */
class FieldErrors<F : Enum<F>>(private val errors: Map<F, Int>) {
  val isValid: Boolean get() = errors.isEmpty()

  /** The first invalid field top to bottom, or null when the form can be sent. */
  val maybeFirst: F? get() = errors.keys.minByOrNull { it.ordinal }

  @StringRes
  operator fun get(field: F): Int? = errors[field]

  /** These errors with [maybeServerError] at [field], where the server refused that field's value. */
  fun withServerError(field: F, @StringRes maybeServerError: Int?): FieldErrors<F> =
    maybeServerError?.let { FieldErrors(errors + (field to it)) } ?: this

  override fun equals(other: Any?): Boolean = other is FieldErrors<*> && other.errors == errors

  override fun hashCode(): Int = errors.hashCode()

  override fun toString(): String = "FieldErrors($errors)"

  companion object {
    /** Builds the errors from (field, error or null) pairs; fields without an error are left out. */
    fun <F : Enum<F>> of(vararg checks: Pair<F, Int?>): FieldErrors<F> =
      FieldErrors(checks.mapNotNull { (field, maybeError) -> maybeError?.let { field to it } }.toMap())
  }
}

/** The two fields of choosing a new recovery passphrase. */
enum class NewPassphraseField { PASSPHRASE, REPEATED }

/**
 * A new passphrase and its repetition: too short at the first field, a missing or different
 * repetition at the second, both at once.
 */
fun newPassphraseErrors(passphrase: String, repeated: String): FieldErrors<NewPassphraseField> = FieldErrors.of(
  NewPassphraseField.PASSPHRASE to passphraseError(passphrase),
  NewPassphraseField.REPEATED to when {
    repeated.isEmpty() -> R.string.passphrase_repeat_missing
    repeated != passphrase -> R.string.passphrases_differ
    else -> null
  },
)

/** A single passphrase field: empty or shorter than [MIN_PASSPHRASE_LENGTH]. */
@StringRes
fun passphraseError(passphrase: String): Int? = when {
  passphrase.isEmpty() -> R.string.passphrase_missing
  passphrase.length < MIN_PASSPHRASE_LENGTH -> R.string.passphrase_too_short
  else -> null
}

/** Whether a failure of the passphrase step is about the typed passphrase, so it goes to that field. */
fun isPassphraseRefusal(@StringRes error: Int): Boolean = error == R.string.error_wrong_passphrase

/** The code a new device shows, as typed on this one: [LINK_CODE_LENGTH] digits. */
@StringRes
fun linkCodeError(typed: String): Int? = when {
  typed.isEmpty() -> R.string.approve_code_missing
  typed.length != LINK_CODE_LENGTH || !typed.all(Char::isDigit) -> R.string.approve_code_incomplete
  else -> null
}

/** The link code of a new computer typed on this device (scanning is the other way). */
@StringRes
fun deviceLinkCodeError(code: String): Int? = if (code.isBlank()) R.string.approve_link_code_missing else null

/** A text that may not be empty: a status, a channel name, an edited message. */
@StringRes
fun requiredTextError(text: String, @StringRes whenEmpty: Int): Int? = if (text.isBlank()) whenEmpty else null

/** The highest member count the "confirm @channel above" setting takes. */
const val MAX_CONFIRM_OVER = 99_999

/** The count as typed, or null when it is no whole number from 0 to [MAX_CONFIRM_OVER]. */
fun confirmOverCount(typed: String): Int? = typed.trim().toIntOrNull()?.takeIf { it in 0..MAX_CONFIRM_OVER }

/** The display name as typed: not empty (the sign-in name has its own button) and no control characters. */
@StringRes
fun displayNameError(typed: String): Int? = when {
  typed.isBlank() -> R.string.profile_name_empty
  typed.any(Char::isISOControl) -> R.string.profile_name_invalid
  else -> null
}

/** Longest description of a problem report, in characters (`MAX_DESCRIPTION_CHARS` of the core). */
const val MAX_REPORT_DESCRIPTION = 4000

/** Largest screenshot of a problem report, in bytes (`MAX_SCREENSHOT_BYTES` of the core). */
const val MAX_REPORT_SCREENSHOT_BYTES = 10L * 1024 * 1024

/** The picture types a problem report takes. */
val REPORT_SCREENSHOT_TYPES = setOf("image/png", "image/jpeg", "image/webp")

/** The fields of "Report a problem", top to bottom. */
enum class ReportField { DESCRIPTION, SCREENSHOT }

/** A screenshot as picked: its type and size, and whether its bytes could be read. */
data class PickedScreenshotInfo(val mediaType: String, val sizeBytes: Long, val isRead: Boolean)

/** The checks of the report form (docs/bug-reports.md), the same as the desktop and web dialog's. */
fun reportErrors(description: String, maybeScreenshot: PickedScreenshotInfo?): FieldErrors<ReportField> = FieldErrors.of(
  ReportField.DESCRIPTION to when {
    description.isBlank() -> R.string.report_error_description
    description.trim().length > MAX_REPORT_DESCRIPTION -> R.string.report_error_description_long
    else -> null
  },
  ReportField.SCREENSHOT to maybeScreenshot?.let { picked ->
    when {
      picked.mediaType !in REPORT_SCREENSHOT_TYPES -> R.string.report_error_screenshot_type
      picked.sizeBytes > MAX_REPORT_SCREENSHOT_BYTES -> R.string.report_error_screenshot_size
      !picked.isRead -> R.string.report_error_screenshot_read
      else -> null
    }
  },
)
