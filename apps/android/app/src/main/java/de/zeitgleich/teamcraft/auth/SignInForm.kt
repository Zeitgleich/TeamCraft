package de.zeitgleich.teamcraft.auth

import androidx.annotation.StringRes
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.ui.forms.FieldErrors
import uniffi.teamcraft_ffi.SignInOption

/** Authenticator apps show six digits; some eight. */
private const val MIN_CODE_DIGITS = 6
private const val MAX_CODE_DIGITS = 8

/*
 * The decisions of the sign-in screen that need neither Android nor the Rust library, so unit
 * tests cover them: what the screen shows for the server's methods, when the local form can be
 * sent, and which text explains a failed local sign-in.
 */

/** One block of the sign-in screen, top to bottom. */
sealed interface SignInItem {
  /** "Sign in with …" through the identity provider (opens the browser). */
  data object SsoButton : SignInItem

  /** "or" between two methods. */
  data object Divider : SignInItem

  /**
   * E-mail, password and one-time code of a local account with the submit button. The server
   * never says whether an account has a second factor (that would confirm the password), so the
   * code field is always there: required with [isCodeRequired], else optional.
   */
  data class LocalForm(val isCodeRequired: Boolean) : SignInItem
}

/** The blocks for [methods] (server order, as checked by the core). */
fun signInItems(methods: List<SignInOption>, isCodeRequired: Boolean): List<SignInItem> {
  val offered = methods.distinct().ifEmpty { listOf(SignInOption.SSO) }
  return offered.flatMapIndexed { index, method ->
    val item = when (method) {
      SignInOption.SSO -> SignInItem.SsoButton
      SignInOption.LOCAL -> SignInItem.LocalForm(isCodeRequired)
    }
    if (index == 0) listOf(item) else listOf(SignInItem.Divider, item)
  }
}

/** Whether the server offers only the identity provider: then confirming it opens the browser at once. */
fun isSsoOnly(methods: List<SignInOption>): Boolean = methods.all { it == SignInOption.SSO }

/** Why a local sign-in didn't go through, in words the user can act on; null when it did. */
@StringRes
fun localSignInErrorText(outcome: LocalLoginOutcome): Int? = when (outcome) {
  is LocalLoginOutcome.SignedIn -> null
  // One text for a wrong e-mail, password or code: no hint which accounts exist.
  LocalLoginOutcome.Rejected -> R.string.local_sign_in_rejected
  LocalLoginOutcome.Disabled -> R.string.local_sign_in_disabled
  LocalLoginOutcome.RateLimited -> R.string.local_sign_in_rate_limited
  LocalLoginOutcome.Unavailable -> R.string.server_error_unavailable
  LocalLoginOutcome.Intercepted -> R.string.server_error_captive_portal
  LocalLoginOutcome.Failed -> R.string.sign_in_failed
}

/** A one-time code as typed: digits only, at most [MAX_CODE_DIGITS]. */
fun cleanTotpCode(input: String): String = input.filter { it in '0'..'9' }.take(MAX_CODE_DIGITS)

/** The fields of the local form, top to bottom. */
enum class SignInField { EMAIL, PASSWORD, CODE }

/**
 * Why the local form can't be sent yet, at each field: an e-mail and a password, and a complete
 * code when the server requires one (an optional code is either empty or complete). The server
 * decides the rest.
 */
fun localFormErrors(email: String, password: String, code: String, isCodeRequired: Boolean): FieldErrors<SignInField> =
  FieldErrors.of(
    SignInField.EMAIL to if (email.isBlank()) R.string.local_email_missing else null,
    SignInField.PASSWORD to if (password.isEmpty()) R.string.local_password_missing else null,
    SignInField.CODE to when {
      code.isEmpty() -> if (isCodeRequired) R.string.local_code_missing else null
      !isTotpCodeComplete(code) -> R.string.local_code_incomplete
      else -> null
    },
  )

/**
 * The field a failed local sign-in belongs to, or null when it belongs to none (the server is
 * down, the account disabled). A wrong e-mail, password or code share one text, so no hint which
 * accounts exist; it goes to the password, the field the user most likely retypes.
 */
fun localSignInErrorField(@StringRes error: Int): SignInField? =
  if (error == R.string.local_sign_in_rejected) SignInField.PASSWORD else null

/** The code to send: `null` when none was typed. */
fun totpCodeToSend(code: String): String? = code.takeIf { it.isNotEmpty() }

private fun isTotpCodeComplete(code: String): Boolean = code.length in MIN_CODE_DIGITS..MAX_CODE_DIGITS && code.all { it in '0'..'9' }
