package de.zeitgleich.teamcraft.auth

import androidx.annotation.StringRes
import java.io.IOException
import de.zeitgleich.teamcraft.R
import net.openid.appauth.AuthorizationException
import uniffi.teamcraft_ffi.AuthorizationErrorKind
import uniffi.teamcraft_ffi.TeamCraftException
import uniffi.teamcraft_ffi.authorizationErrorKind
import uniffi.teamcraft_ffi.shownAuthorizationErrorCode

/** The message [AuthRepository.completeLogin] throws when the browser came back without an answer. */
const val LOGIN_CANCELLED = "login cancelled"

/** What the sign-in screen does after a browser sign-in failed (issue #1063). */
sealed interface SignInFailure {
  /** The provider's `already_logged_in`: a new authorization request, once. */
  data object Retry : SignInFailure

  /** The user closed the browser: the sign-in screen without a message. */
  data object Cancelled : SignInFailure

  /** A message; [maybeCode] is the provider's error code for texts that name it. */
  data class Shown(@StringRes val text: Int, val maybeCode: String? = null) : SignInFailure
}

/** The identity provider's `error` in the redirect: its kind and the code as it may be shown. */
data class ProviderRefusal(val kind: AuthorizationErrorKind, val code: String)

/**
 * The provider's refusal in [failure], if the redirect carried one. The kinds come from the core
 * (`teamcraft-client-api` `oidc_login`), so every app reads a code the same way.
 */
fun providerRefusal(failure: Exception): ProviderRefusal? {
  if (failure !is AuthorizationException || failure.type != AuthorizationException.TYPE_OAUTH_AUTHORIZATION_ERROR) {
    return null
  }
  val error = failure.error.orEmpty()
  return ProviderRefusal(authorizationErrorKind(error), shownAuthorizationErrorCode(error))
}

/**
 * Why a browser sign-in failed, in words the user can act on, or [SignInFailure.Retry] for an
 * `already_logged_in` that wasn't retried yet ([hasRetried]): the browser re-sent a finished login,
 * and a fresh request (new state and PKCE verifier, `prompt=login` kept) shows the login page
 * again. A second one is shown, never looped on.
 */
fun signInFailure(failure: Exception, maybeRefusal: ProviderRefusal?, hasRetried: Boolean): SignInFailure {
  maybeRefusal?.let { return refusalFailure(it, hasRetried) }
  return when {
    failure is AuthorizationException &&
      failure.code == AuthorizationException.GeneralErrors.USER_CANCELED_AUTH_FLOW.code -> SignInFailure.Cancelled
    failure is IllegalStateException && failure.message == LOGIN_CANCELLED -> SignInFailure.Cancelled
    failure is AuthorizationException && failure.code == AuthorizationException.GeneralErrors.NETWORK_ERROR.code ->
      SignInFailure.Shown(R.string.sign_in_network)
    failure is IOException -> SignInFailure.Shown(R.string.sign_in_network)
    failure is TeamCraftException.Network -> SignInFailure.Shown(R.string.sign_in_network)
    failure is AuthorizationException && failure.type == AuthorizationException.TYPE_OAUTH_TOKEN_ERROR ->
      SignInFailure.Shown(R.string.sign_in_rejected)
    else -> SignInFailure.Shown(R.string.sign_in_failed)
  }
}

/**
 * One line for the log: the exception's type and AppAuth code and the provider's cleaned error
 * code. Never the redirect's authorization code, tokens or the provider's free-text description.
 */
fun signInFailureLog(failure: Exception, maybeRefusal: ProviderRefusal?): String {
  val appAuth = (failure as? AuthorizationException)?.let { " type=${it.type} code=${it.code}" }.orEmpty()
  val provider = maybeRefusal?.let { " error=${it.code} kind=${it.kind}" }.orEmpty()
  return "sign-in failed: ${failure.javaClass.simpleName}$appAuth$provider"
}

private fun refusalFailure(refusal: ProviderRefusal, hasRetried: Boolean): SignInFailure =
  when (refusal.kind) {
    AuthorizationErrorKind.ALREADY_SIGNED_IN ->
      if (hasRetried) SignInFailure.Shown(R.string.sign_in_provider_already_signed_in) else SignInFailure.Retry
    AuthorizationErrorKind.ACCESS_DENIED -> SignInFailure.Shown(R.string.sign_in_provider_denied)
    AuthorizationErrorKind.MISCONFIGURED -> SignInFailure.Shown(R.string.sign_in_provider_misconfigured, refusal.code)
    AuthorizationErrorKind.OTHER -> SignInFailure.Shown(R.string.sign_in_provider_refused, refusal.code)
  }
