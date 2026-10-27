package de.zeitgleich.teamcraft.server

import java.io.ByteArrayOutputStream
import java.io.IOException
import java.io.InputStream
import java.net.HttpURLConnection
import java.net.URL
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/** Largest `server-info` answer the app reads; a real one is a few kilobytes. */
private const val MAX_SERVER_INFO_BYTES = 1024 * 1024
private val CONNECT_TIMEOUT_MILLIS = TimeUnit.SECONDS.toMillis(10).toInt()
private val READ_TIMEOUT_MILLIS = TimeUnit.SECONDS.toMillis(15).toInt()

/**
 * Fetches `GET /api/v1/server-info` of the server the user typed (or the managed configuration
 * names) and hands the answer, as received, to the core's checks. Redirects are not followed: a
 * redirect is what a captive portal sends, and the core reports it as one.
 */
object ServerInfoClient {
  /** The raw answer; throws [IOException] when the server can't be reached or answers too much. */
  suspend fun fetch(url: String): ServerInfoAnswer = withContext(Dispatchers.IO) {
    val connection = URL(url).openConnection() as HttpURLConnection
    connection.connectTimeout = CONNECT_TIMEOUT_MILLIS
    connection.readTimeout = READ_TIMEOUT_MILLIS
    connection.instanceFollowRedirects = false
    connection.useCaches = false
    connection.setRequestProperty("Accept", "application/json")
    try {
      val status = connection.responseCode
      // Error answers come on the error stream, which is null when the answer has no body.
      val maybeStream = if (status >= HttpURLConnection.HTTP_BAD_REQUEST) connection.errorStream else connection.inputStream
      val body = maybeStream?.use { readAtMost(it, MAX_SERVER_INFO_BYTES) } ?: ByteArray(0)
      ServerInfoAnswer(status, connection.contentType, body)
    } finally {
      connection.disconnect()
    }
  }
}

/** An HTTP answer as received. */
class ServerInfoAnswer(val status: Int, val maybeContentType: String?, val body: ByteArray)

/** The answer was larger than the app reads; nothing a TeamCraft server sends. */
class ResponseTooLargeException : IOException("response too large")

/** Reads [input] to the end; throws [ResponseTooLargeException] past [maxBytes]. */
internal fun readAtMost(input: InputStream, maxBytes: Int): ByteArray {
  val output = ByteArrayOutputStream()
  val buffer = ByteArray(8 * 1024)
  while (true) {
    val count = input.read(buffer)
    if (count < 0) return output.toByteArray()
    if (output.size() + count > maxBytes) throw ResponseTooLargeException()
    output.write(buffer, 0, count)
  }
}
