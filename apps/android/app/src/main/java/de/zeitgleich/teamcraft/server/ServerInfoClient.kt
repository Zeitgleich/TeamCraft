package de.zeitgleich.teamcraft.server

import java.io.IOException
import java.time.Duration
import de.zeitgleich.teamcraft.core.CoreHttp
import de.zeitgleich.teamcraft.core.header
import uniffi.teamcraft_ffi.HttpFetchHeader

/** Largest `server-info` answer the app reads; a real one is a few kilobytes. */
private const val MAX_SERVER_INFO_BYTES = 1024 * 1024
private val CONNECT_TIMEOUT: Duration = Duration.ofSeconds(10)
private val READ_TIMEOUT: Duration = Duration.ofSeconds(15)

/**
 * Fetches `GET /api/v1/server-info` of the server the user typed (or the managed configuration
 * names) through the core's HTTP client ([CoreHttp]) and hands the answer, as received, to the
 * core's checks. Redirects are not followed: a redirect is what a captive portal sends, and the
 * core reports it as one.
 */
object ServerInfoClient {
  /** The raw answer; throws [IOException] when the server can't be reached or answers too much. */
  suspend fun fetch(url: String): ServerInfoAnswer {
    val answer = CoreHttp.send(
      CoreHttp.request(
        url = url,
        headers = listOf(HttpFetchHeader("Accept", "application/json")),
        maxBodyBytes = MAX_SERVER_INFO_BYTES,
        connectTimeout = CONNECT_TIMEOUT,
        readTimeout = READ_TIMEOUT,
      ),
    )
    return ServerInfoAnswer(answer.status.toInt(), answer.header("Content-Type"), answer.body)
  }
}

/** An HTTP answer as received. */
class ServerInfoAnswer(val status: Int, val maybeContentType: String?, val body: ByteArray)

/** The answer was larger than the app reads; nothing a TeamCraft server sends. */
class ResponseTooLargeException : IOException("response too large")
