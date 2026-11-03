package de.zeitgleich.teamcraft.auth

import java.io.IOException
import de.zeitgleich.teamcraft.R
import net.openid.appauth.AuthorizationException
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.teamcraft_ffi.AuthorizationErrorKind

class SignInFailureTest {
  class ProviderRefusals {
    @Test
    fun alreadyLoggedInIsRetriedOnce() {
      val refusal = ProviderRefusal(AuthorizationErrorKind.ALREADY_SIGNED_IN, "already_logged_in")
      assertEquals(SignInFailure.Retry, signInFailure(redirectError("already_logged_in"), refusal, hasRetried = false))
    }

    @Test
    fun aSecondAlreadyLoggedInIsShownNotRetried() {
      val refusal = ProviderRefusal(AuthorizationErrorKind.ALREADY_SIGNED_IN, "already_logged_in")
      var hasRetried = false
      var retries = 0
      while (signInFailure(redirectError("already_logged_in"), refusal, hasRetried) == SignInFailure.Retry) {
        retries += 1
        hasRetried = true
        assertTrue("no loop", retries <= 1)
      }
      assertEquals(1, retries)
      assertEquals(
        SignInFailure.Shown(R.string.sign_in_provider_already_signed_in),
        signInFailure(redirectError("already_logged_in"), refusal, hasRetried = true),
      )
    }

    @Test
    fun eachKindHasItsText() {
      val cases = listOf(
        ProviderRefusal(AuthorizationErrorKind.ACCESS_DENIED, "access_denied") to
          SignInFailure.Shown(R.string.sign_in_provider_denied),
        ProviderRefusal(AuthorizationErrorKind.MISCONFIGURED, "unauthorized_client") to
          SignInFailure.Shown(R.string.sign_in_provider_misconfigured, "unauthorized_client"),
        ProviderRefusal(AuthorizationErrorKind.OTHER, "server_error") to
          SignInFailure.Shown(R.string.sign_in_provider_refused, "server_error"),
      )
      for ((refusal, expected) in cases) {
        // The retry state doesn't matter for anything but `already_logged_in`.
        assertEquals(expected, signInFailure(redirectError(refusal.code), refusal, hasRetried = false))
        assertEquals(expected, signInFailure(redirectError(refusal.code), refusal, hasRetried = true))
      }
    }

    @Test
    fun theLogLineNamesTypeAndCodeOnly() {
      val failure = redirectError("access_denied", description = "user alice@example.com refused")
      val line = signInFailureLog(failure, ProviderRefusal(AuthorizationErrorKind.ACCESS_DENIED, "access_denied"))
      assertTrue(line, line.contains("type=${AuthorizationException.TYPE_OAUTH_AUTHORIZATION_ERROR}"))
      assertTrue(line, line.contains("error=access_denied"))
      assertFalse(line, line.contains("alice"))
    }
  }

  class OtherFailures {
    @Test
    fun cancellingShowsNothing() {
      assertEquals(
        SignInFailure.Cancelled,
        signInFailure(AuthorizationException.GeneralErrors.USER_CANCELED_AUTH_FLOW, null, hasRetried = false),
      )
      assertEquals(SignInFailure.Cancelled, signInFailure(IllegalStateException(LOGIN_CANCELLED), null, hasRetried = false))
    }

    @Test
    fun networkFailuresSayCheckTheConnection() {
      val expected = SignInFailure.Shown(R.string.sign_in_network)
      assertEquals(expected, signInFailure(AuthorizationException.GeneralErrors.NETWORK_ERROR, null, hasRetried = false))
      assertEquals(expected, signInFailure(IOException("down"), null, hasRetried = false))
    }

    @Test
    fun aRefusedTokenRequestIsRejected() {
      assertEquals(
        SignInFailure.Shown(R.string.sign_in_rejected),
        signInFailure(AuthorizationException.TokenRequestErrors.INVALID_GRANT, null, hasRetried = false),
      )
    }

    @Test
    fun aStateMismatchIsAPlainFailure() {
      assertEquals(
        SignInFailure.Shown(R.string.sign_in_failed),
        signInFailure(AuthorizationException.AuthorizationRequestErrors.STATE_MISMATCH, null, hasRetried = false),
      )
    }
  }
}

/** A provider's refusal as AppAuth reads it from the redirect. */
private fun redirectError(error: String, description: String? = null): AuthorizationException =
  AuthorizationException(
    AuthorizationException.TYPE_OAUTH_AUTHORIZATION_ERROR,
    AuthorizationException.AuthorizationRequestErrors.OTHER.code,
    error,
    description,
    null,
    null,
  )
