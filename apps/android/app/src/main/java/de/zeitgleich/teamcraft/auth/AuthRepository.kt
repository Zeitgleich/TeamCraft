package de.zeitgleich.teamcraft.auth

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
import de.zeitgleich.teamcraft.BuildConfig
import de.zeitgleich.teamcraft.security.KeystoreBox
import de.zeitgleich.teamcraft.security.SealedFile
import de.zeitgleich.teamcraft.server.ServerStore
import de.zeitgleich.teamcraft.server.StoragePaths
import de.zeitgleich.teamcraft.server.isPlainHttpAllowed
import uniffi.teamcraft_ffi.StoredServer

/**
 * OIDC login with AppAuth (authorization code + PKCE) against the active server's identity provider,
 * as the server announced it (`StoredServer`: issuer, client, scopes). The session (refresh token
 * included) is kept sealed with a Keystore key, one file per server; the Rust core asks for access
 * tokens through [accessToken].
 */
class AuthRepository(private val context: Context, private val servers: ServerStore) {
  private val signedIn = MutableStateFlow(false)
  private var isLoaded = false
  private var maybeSession: Session? = null

  /** Whether a session exists (it may still need a refresh). */
  val isSignedIn: StateFlow<Boolean>
    get() {
      session()
      return signedIn
    }

  /** The server the app signs in to; `null` until the user picked one. */
  val server: StoredServer?
    get() = session()?.server

  /** The account key for local data: the OIDC subject. */
  val account: String?
    get() = synchronized(this) { session()?.state?.parsedIdToken?.subject }

  /**
   * Switches to [maybeServer]'s session (signed in if one is stored for it), or to none. The
   * previous server's session file stays; signing out removes it.
   */
  fun activate(maybeServer: StoredServer?) {
    synchronized(this) {
      maybeSession?.service?.dispose()
      maybeSession = maybeServer?.let(::open)
      isLoaded = true
      signedIn.value = maybeSession?.state?.isAuthorized == true
    }
  }

  /** The intent that opens the browser for login (PKCE is added by AppAuth). */
  suspend fun loginIntent(): Intent {
    val session = session() ?: throw IllegalStateException("no server")
    val configuration = discover(session)
    val request = AuthorizationRequest.Builder(
      configuration,
      session.server.clientId,
      ResponseTypeValues.CODE,
      Uri.parse(BuildConfig.OIDC_REDIRECT_URI),
    )
      .setScopes(session.server.scopes)
      .build()
    return session.service.getAuthorizationRequestIntent(request)
  }

  /** Completes the login with the browser's answer. */
  suspend fun completeLogin(result: Intent?) {
    val session = session() ?: throw IllegalStateException("no server")
    val response = result?.let { AuthorizationResponse.fromIntent(it) }
    val error = result?.let { AuthorizationException.fromIntent(it) }
    val fresh = AuthState(response, error)
    if (response == null) throw error ?: IllegalStateException("login cancelled")
    val tokens = suspendCancellableCoroutine { continuation ->
      session.service.performTokenRequest(response.createTokenExchangeRequest()) { tokenResponse, exception ->
        if (tokenResponse != null) {
          continuation.resume(tokenResponse)
        } else {
          continuation.resumeWithException(exception ?: IllegalStateException("no token response"))
        }
      }
    }
    fresh.update(tokens, null)
    synchronized(this) {
      // The user changed the server while the browser was open: this login is for the old one.
      check(maybeSession === session) { "server changed during login" }
      session.state = fresh
    }
    persist(session)
    signedIn.value = true
  }

  /**
   * A valid access token, refreshed if it is about to expire; `null` when signed out. Blocks for
   * at most [REFRESH_TIMEOUT_SECONDS]: called on the Rust engine thread, never on the UI thread.
   */
  fun accessToken(): String? {
    val session = session() ?: return null
    val current = synchronized(this) { session.state }
    if (!current.isAuthorized) return null
    if (!current.needsTokenRefresh) return current.accessToken
    return runBlocking {
      withTimeoutOrNull(TimeUnit.SECONDS.toMillis(REFRESH_TIMEOUT_SECONDS)) { refresh(session, current) }
    }
  }

  /** Ends the active server's session; the server stays selected. */
  fun signOut() {
    session()?.let(::signOut)
  }

  fun dispose() {
    synchronized(this) { maybeSession?.service?.dispose() }
  }

  private fun signOut(session: Session) {
    synchronized(this) { session.state = AuthState() }
    session.file.delete()
    if (maybeSession === session) signedIn.value = false
  }

  /** The active server's session, opened from the stored server on first use. */
  private fun session(): Session? = synchronized(this) {
    if (!isLoaded) {
      maybeSession = servers.active()?.let(::open)
      isLoaded = true
      signedIn.value = maybeSession?.state?.isAuthorized == true
    }
    maybeSession
  }

  private fun open(server: StoredServer): Session {
    val isInsecureHttpAllowed = BuildConfig.DEBUG
    val connections = DevAwareConnectionBuilder(isInsecureHttpAllowed)
    val issuer = Uri.parse(server.issuer)
    val service = AuthorizationService(
      context,
      AppAuthConfiguration.Builder()
        .setConnectionBuilder(connections)
        // AppAuth rejects ID tokens of a plain-HTTP issuer; a development stack's Keycloak is one.
        .setSkipIssuerHttpsCheck(issuer.scheme == "http" && isPlainHttpAllowed(issuer.host, isInsecureHttpAllowed))
        .build(),
    )
    val file = SealedFile(File(context.filesDir, StoragePaths.sessionFile(server.address.storageKey)), KeystoreBox(SESSION_KEY_ALIAS))
    return Session(server, file, service, connections, loadState(file))
  }

  private suspend fun refresh(session: Session, current: AuthState): String? =
    suspendCancellableCoroutine { continuation ->
      current.performActionWithFreshTokens(session.service) { accessToken, _, exception ->
        if (exception != null && exception.type == AuthorizationException.TYPE_OAUTH_TOKEN_ERROR) {
          // The refresh token is no longer valid: the user has to sign in again.
          signOut(session)
        } else {
          persist(session)
        }
        continuation.resume(accessToken)
      }
    }

  private suspend fun discover(session: Session): AuthorizationServiceConfiguration =
    suspendCancellableCoroutine { continuation ->
      AuthorizationServiceConfiguration.fetchFromIssuer(
        Uri.parse(session.server.issuer),
        { configuration, exception ->
          if (configuration != null) {
            continuation.resume(configuration)
          } else {
            continuation.resumeWithException(exception ?: IllegalStateException("no OIDC configuration"))
          }
        },
        session.connections,
      )
    }

  private fun loadState(file: SealedFile): AuthState =
    try {
      file.read()?.let { AuthState.jsonDeserialize(it.decodeToString()) } ?: AuthState()
    } catch (unreadable: Exception) {
      // A session sealed under a lost Keystore key (app data restored, key reset): sign in again.
      file.delete()
      AuthState()
    }

  private fun persist(session: Session) {
    val json = synchronized(this) { session.state.jsonSerializeString() }
    session.file.write(json.encodeToByteArray())
  }

  /** One server's session: its sealed file, AppAuth service and current state. */
  private class Session(
    val server: StoredServer,
    val file: SealedFile,
    val service: AuthorizationService,
    val connections: ConnectionBuilder,
    var state: AuthState,
  )

  /**
   * HTTPS everywhere; plain HTTP only to the device itself, or anywhere in development builds
   * (as `network_security_config.xml` of the build type).
   */
  private class DevAwareConnectionBuilder(private val isInsecureHttpAllowed: Boolean) : ConnectionBuilder {
    override fun openConnection(uri: Uri): HttpURLConnection {
      if (uri.scheme == "http" && isPlainHttpAllowed(uri.host, isInsecureHttpAllowed)) {
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
  }
}
