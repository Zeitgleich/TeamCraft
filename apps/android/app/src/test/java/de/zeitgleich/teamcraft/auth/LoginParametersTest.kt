package de.zeitgleich.teamcraft.auth

import java.util.Locale
import org.junit.Assert.assertEquals
import org.junit.Test

class LoginParametersTest {
  @Test
  fun anInteractiveSignInAsksForTheLoginPageWithHintAndLanguage() {
    assertEquals(
      LoginParameters(maybePrompt = "login", maybeLoginHint = "alice", maybeUiLocales = "de"),
      loginParameters(isLoginPageAlwaysShown = true, maybeLoginHint = " alice ", locale = Locale.GERMAN),
    )
  }

  @Test
  fun theServerSettingAndUnknownValuesDropTheirParameters() {
    assertEquals(
      LoginParameters(maybePrompt = null, maybeLoginHint = null, maybeUiLocales = null),
      loginParameters(isLoginPageAlwaysShown = false, maybeLoginHint = "  ", locale = Locale.ROOT),
    )
  }
}
