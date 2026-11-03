package de.zeitgleich.teamcraft.auth

import java.util.Locale

/**
 * What a sign-in the user starts adds to the authorization request (issue #448): the identity
 * provider's login page also with an existing session there (`prompt=login`, the server's
 * setting), the account last used on this device for this server (`login_hint`), and the app's
 * language (`ui_locales`).
 */
data class LoginParameters(
  val maybePrompt: String?,
  val maybeLoginHint: String?,
  val maybeUiLocales: String?,
)

/** The parameters for a server with these settings, in the app's [locale]. */
fun loginParameters(isLoginPageAlwaysShown: Boolean, maybeLoginHint: String?, locale: Locale): LoginParameters =
  LoginParameters(
    maybePrompt = if (isLoginPageAlwaysShown) PROMPT_LOGIN else null,
    maybeLoginHint = maybeLoginHint?.trim()?.takeIf { it.isNotEmpty() },
    maybeUiLocales = locale.toLanguageTag().takeIf { it.isNotEmpty() && it != "und" },
  )

/** OIDC Core §3.1.2.1. */
private const val PROMPT_LOGIN = "login"
