package de.zeitgleich.teamcraft.auth

import de.zeitgleich.teamcraft.R
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.teamcraft_ffi.SignInOption

class SignInFormTest {
  class Items {
    @Test
    fun onlyTheIdentityProvider() {
      assertEquals(listOf(SignInItem.SsoButton), signInItems(listOf(SignInOption.SSO), isCodeRequired = false))
      assertTrue(isSsoOnly(listOf(SignInOption.SSO)))
    }

    @Test
    fun bothMethodsInTheServersOrderWithADivider() {
      assertEquals(
        listOf(SignInItem.LocalForm(isCodeRequired = true), SignInItem.Divider, SignInItem.SsoButton),
        signInItems(listOf(SignInOption.LOCAL, SignInOption.SSO), isCodeRequired = true),
      )
      assertEquals(
        listOf(SignInItem.SsoButton, SignInItem.Divider, SignInItem.LocalForm(isCodeRequired = false)),
        signInItems(listOf(SignInOption.SSO, SignInOption.LOCAL), isCodeRequired = false),
      )
      assertFalse(isSsoOnly(listOf(SignInOption.SSO, SignInOption.LOCAL)))
    }

    @Test
    fun onlyLocalAccounts() {
      assertEquals(listOf(SignInItem.LocalForm(isCodeRequired = false)), signInItems(listOf(SignInOption.LOCAL), false))
      assertFalse(isSsoOnly(listOf(SignInOption.LOCAL)))
    }

    @Test
    fun duplicatesAndAnEmptyListAreHandled() {
      assertEquals(listOf(SignInItem.SsoButton), signInItems(emptyList(), isCodeRequired = false))
      assertEquals(
        listOf(SignInItem.SsoButton),
        signInItems(listOf(SignInOption.SSO, SignInOption.SSO), isCodeRequired = false),
      )
    }
  }

  class Form {
    @Test
    fun aCompleteFormHasNoErrors() {
      assertTrue(localFormErrors("user@example.com", "secret", "", isCodeRequired = false).isValid)
      assertTrue(localFormErrors("user@example.com", "secret", "123456", isCodeRequired = true).isValid)
      assertTrue(localFormErrors("user@example.com", "secret", "12345678", isCodeRequired = true).isValid)
    }

    @Test
    fun everyMissingFieldIsNamedAtOnceAndTheFirstIsFocused() {
      val errors = localFormErrors("  ", "", "", isCodeRequired = true)
      assertEquals(R.string.local_email_missing, errors[SignInField.EMAIL])
      assertEquals(R.string.local_password_missing, errors[SignInField.PASSWORD])
      assertEquals(R.string.local_code_missing, errors[SignInField.CODE])
      assertEquals(SignInField.EMAIL, errors.maybeFirst)
    }

    @Test
    fun theFirstInvalidFieldFollowsTheFormsOrder() {
      val errors = localFormErrors("user@example.com", "", "123", isCodeRequired = false)
      assertNull(errors[SignInField.EMAIL])
      assertEquals(SignInField.PASSWORD, errors.maybeFirst)
      assertEquals(R.string.local_code_incomplete, errors[SignInField.CODE])
    }

    @Test
    fun anOptionalCodeIsEmptyOrComplete() {
      assertNull(localFormErrors("user@example.com", "secret", "", isCodeRequired = false)[SignInField.CODE])
      assertEquals(
        R.string.local_code_incomplete,
        localFormErrors("user@example.com", "secret", "12345", isCodeRequired = true)[SignInField.CODE],
      )
      assertNull(totpCodeToSend(""))
      assertEquals("123456", totpCodeToSend("123456"))
    }

    @Test
    fun typedCodesKeepDigitsOnly() {
      assertEquals("123456", cleanTotpCode("123 456"))
      assertEquals("12345678", cleanTotpCode("1234-5678-9"))
      assertEquals("", cleanTotpCode("abc"))
    }
  }

  class Errors {
    @Test
    fun everyOutcomeHasItsText() {
      assertNull(localSignInErrorText(LocalLoginOutcome.SignedIn(LocalSession("t", 1, "1"))))
      assertEquals(R.string.local_sign_in_rejected, localSignInErrorText(LocalLoginOutcome.Rejected))
      assertEquals(R.string.local_sign_in_disabled, localSignInErrorText(LocalLoginOutcome.Disabled))
      assertEquals(R.string.local_sign_in_rate_limited, localSignInErrorText(LocalLoginOutcome.RateLimited))
      assertEquals(R.string.server_error_unavailable, localSignInErrorText(LocalLoginOutcome.Unavailable))
      assertEquals(R.string.server_error_captive_portal, localSignInErrorText(LocalLoginOutcome.Intercepted))
      assertEquals(R.string.sign_in_failed, localSignInErrorText(LocalLoginOutcome.Failed))
    }

    @Test
    fun aRejectedSignInGoesToThePasswordOthersToNoField() {
      assertEquals(SignInField.PASSWORD, localSignInErrorField(R.string.local_sign_in_rejected))
      for (text in listOf(R.string.local_sign_in_disabled, R.string.local_sign_in_rate_limited, R.string.server_error_unavailable)) {
        assertNull(localSignInErrorField(text))
      }
    }

    @Test
    fun theRefusalIsShownAtItsFieldBesideTheFormsOwnErrors() {
      val errors = localFormErrors("", "secret", "", isCodeRequired = false)
        .withServerError(SignInField.PASSWORD, R.string.local_sign_in_rejected)
      assertEquals(R.string.local_email_missing, errors[SignInField.EMAIL])
      assertEquals(R.string.local_sign_in_rejected, errors[SignInField.PASSWORD])
      assertFalse(errors.isValid)
    }
  }
}
