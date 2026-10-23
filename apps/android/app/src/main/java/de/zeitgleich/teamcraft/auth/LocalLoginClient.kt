package de.zeitgleich.teamcraft.auth

import android.util.Log
import java.io.IOException
import java.net.HttpURLConnection
import java.net.URL
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import de.zeitgleich.teamcraft.server.readAtMost

private const val TAG = "LocalLogin"
private const val LOGIN_PATH = "/api/v1/auth/local/login"
private const val LOGOUT_PATH = "/api/v1/auth/local/logout"

/** Largest answer the app reads; a real one is well below a kilobyte. */
private const val MAX_ANSWER_BYTES = 64 * 1024
private val CONNECT_TIMEOUT_MILLIS = TimeUnit.SECONDS.toMillis(10).toInt()
private val READ_TIMEOUT_MILLIS = TimeUnit.SECONDS.toMillis(15).toInt()

/**
 * Talks to the local-account endpoints of the account's server. Only the origin the user picked
 * is contacted, and redirects are never followed: credentials must not end up at another host.
 * Passwords, codes and tokens are never logged.
 */
object LocalLoginClient {
  /**
   * Signs in with e-mail and password (and the second-factor code when the server asked for one).
   * Throws [IOException] when the server can't be reached.
   */
  suspend fun login(origin: String, login: String, password: String, maybeTotpCode: String?): LocalLoginOutcome =
    withContext(Dispatchers.IO) {
      val body = buildJsonObject {
        put("login", login)
        put("password", password)
        maybeTotpCode?.let { put("totp_code", it) }
      }.toString().encodeToByteArray()
      val connection = open(origin + LOGIN_PATH)
      try {
        connection.setRequestProperty("Content-Type", "application/json")
        connection.doOutput = true
        connection.setFixedLengthStreamingMode(body.size)
        connection.outputStream.use { it.write(body) }
        val status = connection.responseCode
        val maybeStream = if (status >= HttpURLConnection.HTTP_BAD_REQUEST) connection.errorStream else connection.inputStream
        val answer = maybeStream?.use { readAtMost(it, MAX_ANSWER_BYTES) } ?: ByteArray(0)
        val maybeServerDate = connection.getHeaderFieldDate("Date", 0L).takeIf { it > 0L }
        localLoginOutcome(status, answer, maybeServerDate, System.currentTimeMillis())
      } finally {
        connection.disconnect()
      }
    }

  /** Ends the session on the server; best effort (the token is deleted on the device either way). */
  fun logout(origin: String, session: LocalSession) {
    val connection = try {
      open(origin + LOGOUT_PATH)
    } catch (failure: IOException) {
      Log.i(TAG, "logout not sent: ${failure.javaClass.simpleName}")
      return
    }
    try {
      connection.setRequestProperty("Authorization", "Bearer ${session.accessToken}")
      connection.doOutput = true
      connection.setFixedLengthStreamingMode(0)
      connection.outputStream.close()
      Log.i(TAG, "logout answered ${connection.responseCode}")
    } catch (failure: IOException) {
      Log.i(TAG, "logout not sent: ${failure.javaClass.simpleName}")
    } finally {
      connection.disconnect()
    }
  }

  private fun open(url: String): HttpURLConnection =
    (URL(url).openConnection() as HttpURLConnection).apply {
      requestMethod = "POST"
      connectTimeout = CONNECT_TIMEOUT_MILLIS
      readTimeout = READ_TIMEOUT_MILLIS
      instanceFollowRedirects = false
      useCaches = false
      setRequestProperty("Accept", "application/json")
    }
}
