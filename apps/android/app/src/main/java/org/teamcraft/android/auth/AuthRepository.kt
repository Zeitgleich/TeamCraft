package org.teamcraft.android.auth

import android.content.Context
import android.content.Intent
import android.net.Uri
import java.io.File
import java.net.HttpURLConnection
import java.net.URL
import java.util.concurrent.TimeUnit
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.withTimeoutOrNull
import net.openid.appauth.AppAuthConfiguration
import net.openid.appauth.AuthState
import net.openid.appauth.AuthorizationException
import net.openid.appauth.AuthorizationRequest
import net.openid.appauth.AuthorizationResponse
import net.openid.appauth.AuthorizationService
import net.openid.appauth.AuthorizationServiceConfiguration
import net.openid.appauth.ResponseTypeValues
import net.openid.appauth.connectivity.ConnectionBuilder
import net.openid.appauth.connectivity.DefaultConnectionBuilder
import org.teamcraft.android.BuildConfig
import org.teamcraft.android.security.KeystoreBox
import org.teamcraft.android.security.SealedFile

/**
 * OIDC login with AppAuth (authorization code + PKCE) against the configured issuer. The session
 * (refresh token included) is kept sealed with a Keystore key; the Rust core asks for access tokens
 * through [accessToken].
 */
class AuthRepository(context: Context) {
  private val sessionFile = SealedFile(
    File(context.filesDir, "auth/session.bin"),
    KeystoreBox(SESSION_KEY_ALIAS),
  )
  private val service = AuthorizationService(
    context,
    AppAuthConfiguration.Builder().setConnectionBuilder(DevAwareConnectionBuilder).build(),
  )
  private var state: AuthState = loadState()
  private val signedIn = MutableStateFlow(state.isAuthorized)

  /** Whether a session exists (it may still need a refresh). */
  val isSignedIn: StateFlow<Boolean> = signedIn

  /** The account key for local data: the OIDC subject. */
  val account: String?
    get() = state.parsedIdToken?.subject

  /** The intent that opens the browser for login (PKCE is added by AppAuth). */
  suspend fun loginIntent(): Intent {
    val configuration = discover()
    val request = AuthorizationRequest.Builder(
      configuration,
      BuildConfig.OIDC_CLIENT_ID,
      ResponseTypeValues.CODE,
      Uri.parse(BuildConfig.OIDC_REDIRECT_URI),
    )
      .setScopes("openid", "profile", "email")
      .build()
    return service.getAuthorizationRequestIntent(request)
  }

  /** Completes the login with the browser's answer. */
  suspend fun completeLogin(result: Intent?) {
    val response = result?.let { AuthorizationResponse.fromIntent(it) }
    val error = result?.let { AuthorizationException.fromIntent(it) }
    val fresh = AuthState(response, error)
    if (response == null) throw error ?: IllegalStateException("login cancelled")
    val tokens = suspendCancellableCoroutine { continuation ->
      service.performTokenRequest(response.createTokenExchangeRequest()) { tokenResponse, exception ->
        if (tokenResponse != null) {
          continuation.resume(tokenResponse)
        } else {
          continuation.resumeWithException(exception ?: IllegalStateException("no token response"))
        }
      }
    }
    fresh.update(tokens, null)
    synchronized(this) { state = fresh }
    persist()
    signedIn.value = true
  }

  /**
   * A valid access token, refreshed if it is about to expire; `null` when signed out. Blocks for
   * at most [REFRESH_TIMEOUT_SECONDS]: called on the Rust engine thread, never on the UI thread.
   */
  fun accessToken(): String? {
    val current = synchronized(this) { state }
    if (!current.isAuthorized) return null
    if (!current.needsTokenRefresh) return current.accessToken
    return runBlocking {
      withTimeoutOrNull(TimeUnit.SECONDS.toMillis(REFRESH_TIMEOUT_SECONDS)) { refresh(current) }
    }
  }

  fun signOut() {
    synchronized(this) { state = AuthState() }
    sessionFile.delete()
    signedIn.value = false
  }

  fun dispose() {
    service.dispose()
  }

  private suspend fun refresh(current: AuthState): String? =
    suspendCancellableCoroutine { continuation ->
      current.performActionWithFreshTokens(service) { accessToken, _, exception ->
        if (exception != null && exception.type == AuthorizationException.TYPE_OAUTH_TOKEN_ERROR) {
          // The refresh token is no longer valid: the user has to sign in again.
          signOut()
        } else {
          persist()
        }
        continuation.resume(accessToken)
      }
    }

  private suspend fun discover(): AuthorizationServiceConfiguration =
    suspendCancellableCoroutine { continuation ->
      AuthorizationServiceConfiguration.fetchFromIssuer(
        Uri.parse(BuildConfig.OIDC_ISSUER),
        { configuration, exception ->
          if (configuration != null) {
            continuation.resume(configuration)
          } else {
            continuation.resumeWithException(exception ?: IllegalStateException("no OIDC configuration"))
          }
        },
        DevAwareConnectionBuilder,
      )
    }

  private fun loadState(): AuthState =
    try {
      sessionFile.read()?.let { AuthState.jsonDeserialize(it.decodeToString()) } ?: AuthState()
    } catch (unreadable: Exception) {
      // A session sealed under a lost Keystore key (app data restored, key reset): sign in again.
      sessionFile.delete()
      AuthState()
    }

  private fun persist() {
    val json = synchronized(this) { state.jsonSerializeString() }
    sessionFile.write(json.encodeToByteArray())
  }

  /** HTTPS everywhere, plain HTTP only to the development stack (as network_security_config). */
  private object DevAwareConnectionBuilder : ConnectionBuilder {
    override fun openConnection(uri: Uri): HttpURLConnection {
      if (uri.scheme == "http" && uri.host in DEV_HOSTS) {
        return (URL(uri.toString()).openConnection() as HttpURLConnection).apply {
          connectTimeout = TimeUnit.SECONDS.toMillis(15).toInt()
          readTimeout = TimeUnit.SECONDS.toMillis(10).toInt()
          instanceFollowRedirects = false
        }
      }
      return DefaultConnectionBuilder.INSTANCE.openConnection(uri)
    }
  }

  private companion object {
    const val SESSION_KEY_ALIAS = "teamcraft_session"
    const val REFRESH_TIMEOUT_SECONDS = 10L
    val DEV_HOSTS = setOf("10.0.2.2", "127.0.0.1", "localhost")
  }
}
