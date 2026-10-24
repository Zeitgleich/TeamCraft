package de.zeitgleich.teamcraft.auth

import android.content.Context
import android.util.Log
import java.io.File
import java.io.IOException
import java.net.HttpURLConnection
import java.net.URL
import java.net.URLEncoder
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import net.openid.appauth.AuthState
import de.zeitgleich.teamcraft.BuildConfig
import de.zeitgleich.teamcraft.security.KeystoreBox
import de.zeitgleich.teamcraft.security.SealedFile
import de.zeitgleich.teamcraft.server.LEGACY_KEYS
import de.zeitgleich.teamcraft.server.LEGACY_SESSION
import de.zeitgleich.teamcraft.server.LEGACY_STORE
import de.zeitgleich.teamcraft.server.ServerStore
import de.zeitgleich.teamcraft.server.StoragePaths
import de.zeitgleich.teamcraft.server.readAtMost
import uniffi.teamcraft_ffi.AppConfig
import uniffi.teamcraft_ffi.EventListener
import uniffi.teamcraft_ffi.LegacyCleanupAction
import uniffi.teamcraft_ffi.LegacyProviderInfo
import uniffi.teamcraft_ffi.LegacySignOutException
import uniffi.teamcraft_ffi.StoredServer
import uniffi.teamcraft_ffi.TeamCraft
import uniffi.teamcraft_ffi.TokenProvider
import uniffi.teamcraft_ffi.isLegacyProvider
import uniffi.teamcraft_ffi.legacyCleanupAction
import uniffi.teamcraft_ffi.legacyDiscoveryUrl
import uniffi.teamcraft_ffi.legacyTokenRevocation

private const val TAG = "LegacySignOut"
private const val SESSION_KEY_ALIAS = "teamcraft_session"
private const val DATABASE_KEY_ALIAS = "teamcraft_database"
private const val MAX_ANSWER_BYTES = 256 * 1024
private val CONNECT_TIMEOUT_MILLIS = TimeUnit.SECONDS.toMillis(10).toInt()
private val READ_TIMEOUT_MILLIS = TimeUnit.SECONDS.toMillis(15).toInt()

/**
 * Signs out the session of builds from before the server entry, which `ServerStore` moved aside
 * (#224): its push registration is removed with the old session once the user entered the server
 * it belonged to, its refresh token revoked at its identity provider, then everything deleted.
 * Which provider may get the token and how long to wait for the server is decided in the core
 * (`teamcraft-client-api` `legacy_session`). Best effort: a failed request never keeps the old
 * session beyond the wait. Tokens are never logged.
 */
class LegacySignOut(private val context: Context, private val servers: ServerStore) {
  private val lock = Mutex()

  /** Runs on app start and after the user confirmed a server; cheap when nothing is left. */
  suspend fun run(maybeActive: StoredServer?) = lock.withLock {
    withContext(Dispatchers.IO) {
      val legacy = servers.legacyDir() ?: return@withContext
      try {
        if (signOut(legacy, maybeActive)) servers.removeLegacy()
      } catch (failure: Exception) {
        // Unreadable leftovers (a lost Keystore key, a broken store): nothing can be signed out.
        Log.w(TAG, "old session deleted without sign-out: ${failure.javaClass.simpleName}")
        servers.removeLegacy()
      }
    }
  }

  /** False while the old session waits for its server. */
  private suspend fun signOut(legacy: File, maybeActive: StoredServer?): Boolean {
    val sessionFile = File(legacy, LEGACY_SESSION)
    val state = SealedFile(sessionFile, KeystoreBox(SESSION_KEY_ALIAS)).read()
      ?.let { AuthState.jsonDeserialize(it.decodeToString()) }
      ?: return true
    val refreshToken = state.refreshToken ?: return true
    val configuration = state.authorizationServiceConfiguration
    val clientId = state.lastAuthorizationResponse?.request?.clientId ?: return true
    val provider = LegacyProviderInfo(
      maybeIssuer = configuration?.discoveryDoc?.issuer,
      maybeTokenEndpoint = configuration?.tokenEndpoint?.toString(),
      clientId = clientId,
    )
    val allowHttp = BuildConfig.DEBUG
    val maybeServerDiscovery = maybeActive?.let { fetch(legacyDiscoveryUrl(it.issuer)) }
    val isMatching = maybeActive != null && maybeServerDiscovery != null &&
      isLegacyProvider(provider, maybeActive.issuer, maybeActive.clientId, maybeServerDiscovery, allowHttp)
    return when (legacyCleanupAction(System.currentTimeMillis(), servers.legacyMovedAt(), isMatching)) {
      LegacyCleanupAction.WAIT_FOR_SERVER -> false
      LegacyCleanupAction.UNREGISTER_PUSH_THEN_REVOKE -> {
        // Invariant: matching means a server and its discovery document are at hand.
        val server = checkNotNull(maybeActive)
        val discovery = checkNotNull(maybeServerDiscovery)
        val latest = unregisterPush(legacy, state, server, refreshToken, clientId) ?: refreshToken
        revoke(provider, server.issuer, discovery, latest, allowHttp)
        true
      }
      LegacyCleanupAction.REVOKE_AND_DELETE -> {
        provider.maybeIssuer?.let { issuer ->
          fetch(legacyDiscoveryUrl(issuer))?.let { revoke(provider, issuer, it, refreshToken, allowHttp) }
        }
        true
      }
    }
  }

  /**
   * Opens the old store with the old key and unregisters its device's push at [server], with an
   * access token from the old refresh token. Returns the refresh token the provider rotated to.
   */
  private suspend fun unregisterPush(
    legacy: File,
    state: AuthState,
    server: StoredServer,
    refreshToken: String,
    clientId: String,
  ): String? {
    val subject = state.parsedIdToken?.subject ?: return null
    val account = StoragePaths.storeName(subject)
    val key = SealedFile(File(legacy, "$LEGACY_KEYS/$account.key"), KeystoreBox(DATABASE_KEY_ALIAS)).read() ?: return null
    val tokenEndpoint = state.authorizationServiceConfiguration?.tokenEndpoint?.toString() ?: return null
    val tokens = refresh(tokenEndpoint, refreshToken, clientId) ?: return null
    val core = TeamCraft(
      AppConfig(
        serverUrl = server.address.origin,
        dataDir = File(legacy, LEGACY_STORE).absolutePath,
        account = account,
        databaseKey = key,
        pushGatewayUrl = BuildConfig.PUSH_GATEWAY_URL.ifEmpty { null },
      ),
      StaticTokens(tokens.first),
      NoChanges(),
    )
    try {
      core.unregisterPush()
      Log.i(TAG, "unregistered the old device's push")
    } catch (failure: Exception) {
      Log.i(TAG, "could not unregister the old device's push: ${failure.javaClass.simpleName}")
    } finally {
      core.close()
    }
    return tokens.second
  }

  private fun revoke(provider: LegacyProviderInfo, issuer: String, discovery: ByteArray, refreshToken: String, allowHttp: Boolean) {
    val request = try {
      legacyTokenRevocation(provider, issuer, discovery, refreshToken, allowHttp)
    } catch (refused: LegacySignOutException) {
      Log.i(TAG, "old refresh token not revoked: ${refused.javaClass.simpleName}")
      return
    }
    val body = request.form.joinToString("&") { "${encode(it.name)}=${encode(it.value)}" }
    val status = post(request.url, body)?.first
    Log.i(TAG, "revocation answered ${status ?: "nothing"}")
  }

  /** (access token, rotated refresh token) from a refresh at [tokenEndpoint]; `null` when refused. */
  private fun refresh(tokenEndpoint: String, refreshToken: String, clientId: String): Pair<String, String?>? {
    val body = listOf("grant_type" to "refresh_token", "refresh_token" to refreshToken, "client_id" to clientId)
      .joinToString("&") { "${encode(it.first)}=${encode(it.second)}" }
    val (status, answer) = post(tokenEndpoint, body) ?: return null
    if (status != HttpURLConnection.HTTP_OK) return null
    return try {
      val fields = Json.parseToJsonElement(answer.decodeToString()).jsonObject
      val access = fields["access_token"]?.jsonPrimitive?.content ?: return null
      access to fields["refresh_token"]?.jsonPrimitive?.content
    } catch (unreadable: IllegalArgumentException) {
      null
    }
  }

  private fun fetch(url: String): ByteArray? = request(url, "GET", null)?.takeIf { it.first == HttpURLConnection.HTTP_OK }?.second

  private fun post(url: String, form: String): Pair<Int, ByteArray>? = request(url, "POST", form.encodeToByteArray())

  /** (status, body); `null` when the request didn't get through. Never follows redirects. */
  private fun request(url: String, method: String, maybeForm: ByteArray?): Pair<Int, ByteArray>? {
    val connection = try {
      (URL(url).openConnection() as HttpURLConnection).apply {
        requestMethod = method
        connectTimeout = CONNECT_TIMEOUT_MILLIS
        readTimeout = READ_TIMEOUT_MILLIS
        instanceFollowRedirects = false
        useCaches = false
      }
    } catch (failure: IOException) {
      return null
    }
    return try {
      maybeForm?.let { form ->
        connection.setRequestProperty("Content-Type", "application/x-www-form-urlencoded")
        connection.doOutput = true
        connection.setFixedLengthStreamingMode(form.size)
        connection.outputStream.use { it.write(form) }
      }
      val status = connection.responseCode
      val maybeStream = if (status >= HttpURLConnection.HTTP_BAD_REQUEST) connection.errorStream else connection.inputStream
      status to (maybeStream?.use { readAtMost(it, MAX_ANSWER_BYTES) } ?: ByteArray(0))
    } catch (failure: IOException) {
      null
    } finally {
      connection.disconnect()
    }
  }

  private fun encode(value: String): String = URLEncoder.encode(value, Charsets.UTF_8.name())

  private class StaticTokens(private val token: String) : TokenProvider {
    override fun accessToken(): String? = token
  }

  private class NoChanges : EventListener {
    override fun onChange() {}
  }
}
