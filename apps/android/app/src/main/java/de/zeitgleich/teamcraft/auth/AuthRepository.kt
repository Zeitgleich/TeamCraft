package de.zeitgleich.teamcraft.auth

import android.content.Context
import android.content.Intent
import android.net.Uri
import androidx.browser.customtabs.ExperimentalEphemeralBrowsing
import java.io.File
import java.net.HttpURLConnection
import java.net.URL
import java.util.Locale
import java.util.concurrent.TimeUnit
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch
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
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.security.KeystoreBox
import de.zeitgleich.teamcraft.security.SealedFile
import de.zeitgleich.teamcraft.server.ServerStore
import de.zeitgleich.teamcraft.server.StoragePaths
import de.zeitgleich.teamcraft.server.isPlainHttpAllowed
import uniffi.teamcraft_ffi.StoredServer
import uniffi.teamcraft_ffi.loginHintFromIdToken
import uniffi.teamcraft_ffi.storedServerWithLoginHint

/**
 * OIDC login with AppAuth (authorization code + PKCE) against the active server's identity provider,
 * as the server announced it (`StoredServer`: issuer, client, scopes), or a local account's sign-in
 * with e-mail and password ([signInLocal]). Either session is kept sealed with a Keystore key, one
 * file per server; the Rust core asks for access tokens through [accessToken].
 */
class AuthRepository(private val context: Context, private val servers: ServerStore) {
  /** For the best-effort logout of a local session, which must not hold up signing out. */
  private val background = CoroutineScope(SupervisorJob() + Dispatchers.IO)
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

  /** The account key for local data: the OIDC subject, or the local account's user ID. */
  val account: String?
    get() = synchronized(this) {
      val session = session() ?: return null
      session.maybeLocal?.userId ?: session.state.parsedIdToken?.subject
    }

  /**
   * Switches to [maybeServer]'s session (signed in if one is stored for it), or to none. The
   * previous server's session file stays; signing out removes it.
   */
  fun activate(maybeServer: StoredServer?) {
    synchronized(this) {
      maybeSession?.service?.dispose()
      maybeSession = maybeServer?.let(::open)
      isLoaded = true
      signedIn.value = maybeSession?.isSignedIn() == true
    }
  }

  /**
   * Signs in to the active server's local account. On success the session replaces any identity
   * provider session of this server. Throws [IOException][java.io.IOException] when the server
   * can't be reached.
   */
  suspend fun signInLocal(login: String, password: String, maybeTotpCode: String?): LocalLoginOutcome {
    val session = session() ?: throw IllegalStateException("no server")
    val outcome = LocalLoginClient.login(session.server.address.origin, login, password, maybeTotpCode)
    if (outcome !is LocalLoginOutcome.SignedIn) return outcome
    synchronized(this) {
      // The user changed the server while the request ran: this session is for the old one.
      check(maybeSession === session) { "server changed during login" }
      session.maybeLocal = outcome.session
      session.state = AuthState()
    }
    session.file.delete()
    session.localFile.write(outcome.session.encode())
    signedIn.value = true
    return outcome
  }

  /**
   * The intent that opens the browser for login (PKCE is added by AppAuth), with the last account
   * and the app's language (issue #448). Where the server asks for the identity provider's login
   * page ([maybeLoginPageAlwaysShown], the sign-in screen's fresh answer; else the stored server's
   * setting), it sends `prompt=login` and opens an ephemeral Custom Tab where the browser supports
   * one, so the browser's provider session isn't reused. Otherwise silent single sign-on works.
   */
  @OptIn(ExperimentalEphemeralBrowsing::class)
  suspend fun loginIntent(maybeLoginPageAlwaysShown: Boolean?): Intent {
    val session = session() ?: throw IllegalStateException("no server")
    val configuration = discover(session)
    val server = synchronized(this) { session.server }
    val isLoginPageShown = maybeLoginPageAlwaysShown ?: server.isLoginPageAlwaysShown
    val parameters = loginParameters(
      isLoginPageShown,
      server.maybeLoginHint,
      Locale.forLanguageTag(context.getString(R.string.app_language)),
    )
    val request = AuthorizationRequest.Builder(
      configuration,
      server.clientId,
      ResponseTypeValues.CODE,
      Uri.parse(BuildConfig.OIDC_REDIRECT_URI),
    )
      .setScopes(server.scopes)
      .setPrompt(parameters.maybePrompt)
      .setLoginHint(parameters.maybeLoginHint)
      .setUiLocales(parameters.maybeUiLocales)
      .build()
    val tab = session.service.createCustomTabsIntentBuilder(request.toUri())
      .setEphemeralBrowsingEnabled(isLoginPageShown)
      .build()
    return session.service.getAuthorizationRequestIntent(request, tab)
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
      session.maybeLocal = null
    }
    session.localFile.delete()
    persist(session)
    rememberLoginHint(session, tokens.idToken)
    signedIn.value = true
  }

  /** The account the ID token names, prefilled at this server's next sign-in (issue #448). */
  private fun rememberLoginHint(session: Session, maybeIdToken: String?) {
    val hint = maybeIdToken?.let(::loginHintFromIdToken) ?: return
    val server = synchronized(this) { session.server }
    if (server.maybeLoginHint == hint) return
    val updated = storedServerWithLoginHint(server.profileJson, hint) ?: return
    synchronized(this) { session.server = updated }
    servers.save(updated)
  }

  /**
   * A valid access token, refreshed if it is about to expire; `null` when signed out. A local
   * session has no refresh: once it expired the user is signed out. Blocks for at most
   * [REFRESH_TIMEOUT_SECONDS]: called on the Rust engine thread, never on the UI thread.
   */
  fun accessToken(): String? {
    val session = session() ?: return null
    synchronized(this) { session.maybeLocal }?.let { local ->
      if (local.isValid(System.currentTimeMillis())) return local.accessToken
      signOut(session)
      return null
    }
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

  /** Ends either session; a local one is also ended on the server, in the background. */
  private fun signOut(session: Session) {
    val maybeLocal = synchronized(this) {
      val local = session.maybeLocal
      session.state = AuthState()
      session.maybeLocal = null
      local
    }
    session.file.delete()
    session.localFile.delete()
    if (maybeSession === session) signedIn.value = false
    // An expired token is already dead on the server; only a live one is worth a request.
    maybeLocal?.takeIf { it.isValid(System.currentTimeMillis()) }?.let { local ->
      background.launch { LocalLoginClient.logout(session.server.address.origin, local) }
    }
  }

  /** The active server's session, opened from the stored server on first use. */
  private fun session(): Session? = synchronized(this) {
    if (!isLoaded) {
      maybeSession = servers.active()?.let(::open)
      isLoaded = true
      signedIn.value = maybeSession?.isSignedIn() == true
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
    val storageKey = server.address.storageKey
    val file = SealedFile(File(context.filesDir, StoragePaths.sessionFile(storageKey)), KeystoreBox(SESSION_KEY_ALIAS))
    val localFile = SealedFile(File(context.filesDir, StoragePaths.localSessionFile(storageKey)), KeystoreBox(SESSION_KEY_ALIAS))
    return Session(server, file, localFile, service, connections, loadState(file), loadLocal(localFile))
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

  /** The stored local session while it is valid; an expired or unreadable one is deleted. */
  private fun loadLocal(file: SealedFile): LocalSession? {
    val maybeLocal = try {
      file.read()?.let(LocalSession::decode)
    } catch (unreadable: Exception) {
      // Sealed under a lost Keystore key: sign in again.
      null
    }
    if (maybeLocal == null || !maybeLocal.isValid(System.currentTimeMillis())) {
      file.delete()
      return null
    }
    return maybeLocal
  }

  private fun persist(session: Session) {
    val json = synchronized(this) { session.state.jsonSerializeString() }
    session.file.write(json.encodeToByteArray())
  }

  /**
   * One server's session: its sealed files, AppAuth service and current state. At most one of
   * [state] (identity provider) and [maybeLocal] (local account) is signed in.
   */
  private class Session(
    @Volatile var server: StoredServer,
    val file: SealedFile,
    val localFile: SealedFile,
    val service: AuthorizationService,
    val connections: ConnectionBuilder,
    var state: AuthState,
    var maybeLocal: LocalSession?,
  ) {
    fun isSignedIn(): Boolean = maybeLocal != null || state.isAuthorized
  }

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
