package de.zeitgleich.teamcraft.auth

import android.util.Log
import java.io.IOException
import java.time.Duration
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import de.zeitgleich.teamcraft.core.CoreHttp
import de.zeitgleich.teamcraft.core.header
import de.zeitgleich.teamcraft.core.httpDateMillis
import uniffi.teamcraft_ffi.HttpFetchHeader
import uniffi.teamcraft_ffi.HttpFetchMethod
import uniffi.teamcraft_ffi.HttpFetchRequest

private const val TAG = "LocalLogin"
private const val LOGIN_PATH = "/api/v1/auth/local/login"
private const val LOGOUT_PATH = "/api/v1/auth/local/logout"

/** Largest answer the app reads; a real one is well below a kilobyte. */
private const val MAX_ANSWER_BYTES = 64 * 1024
private val CONNECT_TIMEOUT: Duration = Duration.ofSeconds(10)
private val READ_TIMEOUT: Duration = Duration.ofSeconds(15)

/**
 * Talks to the local-account endpoints of the account's server, through the core's HTTP client
 * ([CoreHttp]). Only the origin the user picked is contacted, and redirects are never followed:
 * credentials must not end up at another host. Passwords, codes and tokens are never logged.
 */
object LocalLoginClient {
  /**
   * Signs in with e-mail and password (and the second-factor code when the server asked for one).
   * Throws [IOException] when the server can't be reached.
   */
  suspend fun login(origin: String, login: String, password: String, maybeTotpCode: String?): LocalLoginOutcome {
    val body = buildJsonObject {
      put("login", login)
      put("password", password)
      maybeTotpCode?.let { put("totp_code", it) }
    }.toString().encodeToByteArray()
    val answer = CoreHttp.send(post(origin + LOGIN_PATH, body, HttpFetchHeader("Content-Type", "application/json")))
    val maybeServerDate = answer.header("Date")?.let(::httpDateMillis)
    return localLoginOutcome(answer.status.toInt(), answer.body, maybeServerDate, System.currentTimeMillis())
  }

  /** Ends the session on the server; best effort (the token is deleted on the device either way). */
  suspend fun logout(origin: String, session: LocalSession) {
    val authorization = HttpFetchHeader("Authorization", "Bearer ${session.accessToken}")
    try {
      val answer = CoreHttp.send(post(origin + LOGOUT_PATH, ByteArray(0), authorization))
      Log.i(TAG, "logout answered ${answer.status}")
    } catch (failure: IOException) {
      Log.i(TAG, "logout not sent: ${failure.javaClass.simpleName}")
    }
  }

  private fun post(url: String, body: ByteArray, header: HttpFetchHeader): HttpFetchRequest =
    CoreHttp.request(
      url = url,
      method = HttpFetchMethod.POST,
      headers = listOf(HttpFetchHeader("Accept", "application/json"), header),
      body = body,
      maxBodyBytes = MAX_ANSWER_BYTES,
      connectTimeout = CONNECT_TIMEOUT,
      readTimeout = READ_TIMEOUT,
    )
}
