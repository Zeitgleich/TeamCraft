package de.zeitgleich.teamcraft.core

import java.io.IOException
import java.net.MalformedURLException
import java.net.SocketTimeoutException
import java.net.UnknownServiceException
import java.time.Duration
import java.time.ZonedDateTime
import java.time.format.DateTimeFormatter
import javax.net.ssl.SSLHandshakeException
import de.zeitgleich.teamcraft.BuildConfig
import de.zeitgleich.teamcraft.server.ResponseTooLargeException
import uniffi.teamcraft_ffi.HttpFetchException
import uniffi.teamcraft_ffi.HttpFetchHeader
import uniffi.teamcraft_ffi.HttpFetchMethod
import uniffi.teamcraft_ffi.HttpFetchRedirects
import uniffi.teamcraft_ffi.HttpFetchRequest
import uniffi.teamcraft_ffi.HttpFetchResponse
import uniffi.teamcraft_ffi.httpFetch

/**
 * The app's own HTTP requests go through the core's HTTP client (issue #433): the identity
 * provider's ([CoreHttpConnection] for AppAuth), a local account's sign-in, the server check, brand
 * images and the video codec download. The platform's `HttpURLConnection` tries a host's addresses
 * one after another and waits out its connect timeout on a black-holed IPv6 route; the core races
 * IPv6 and IPv4 (#420) and uses the proxy and CAs [NetworkTrust] hands over (#157), like the rest of
 * the app's connections. The network security config doesn't apply to it: the core refuses plain
 * HTTP itself (`allowInsecureHttp`).
 */
object CoreHttp {
  /**
   * Sends [request] and reads the answer, whatever its status. A failure on the way is the
   * [IOException] the platform's client throws for it ([asIoException]); cancelling the coroutine
   * cancels the request.
   */
  suspend fun send(request: HttpFetchRequest): HttpFetchResponse {
    // The core's TLS trusts the device's CAs once they are handed over.
    NetworkTrust.awaitTrustedCertificates()
    return try {
      httpFetch(request)
    } catch (failure: HttpFetchException) {
      throw failure.asIoException()
    }
  }

  /**
   * A request with the app's rules: redirects are the answer unless [redirects] says otherwise,
   * and plain HTTP goes only to the device itself, or anywhere in development builds.
   */
  fun request(
    url: String,
    maxBodyBytes: Int,
    connectTimeout: Duration,
    readTimeout: Duration,
    method: HttpFetchMethod = HttpFetchMethod.GET,
    headers: List<HttpFetchHeader> = emptyList(),
    body: ByteArray = ByteArray(0),
    redirects: HttpFetchRedirects = HttpFetchRedirects.DONT_FOLLOW,
    allowInsecureHttp: Boolean = BuildConfig.DEBUG,
  ): HttpFetchRequest = HttpFetchRequest(
    method = method,
    url = url,
    headers = headers,
    body = body,
    redirects = redirects,
    allowInsecureHttp = allowInsecureHttp,
    connectTimeout = connectTimeout,
    readTimeout = readTimeout,
    maxBodyBytes = maxBodyBytes.toULong(),
  )
}

/** The last value of the header [name] (any case), as the platform's client answers. */
fun HttpFetchResponse.header(name: String): String? =
  headers.lastOrNull { it.name.equals(name, ignoreCase = true) }?.value

/**
 * The core's failure as the exception the platform's client throws for it, so AppAuth (which
 * handles only [IOException]s) and [transportErrorText][de.zeitgleich.teamcraft.server.transportErrorText]
 * keep telling a refused certificate, refused plain HTTP and an oversized answer apart from an
 * unreachable server.
 */
fun HttpFetchException.asIoException(): IOException {
  val failure = when (this) {
    is HttpFetchException.InvalidRequest -> MalformedURLException(message)
    is HttpFetchException.PlainHttpRefused -> UnknownServiceException(message)
    is HttpFetchException.UntrustedCertificate -> SSLHandshakeException(message)
    is HttpFetchException.TimedOut -> SocketTimeoutException(message)
    is HttpFetchException.TooLarge -> ResponseTooLargeException()
    is HttpFetchException.Network -> IOException(message)
  }
  failure.initCause(this)
  return failure
}

/** An HTTP date (the `Date` header) in milliseconds since the epoch; null when it isn't one. */
fun httpDateMillis(value: String): Long? =
  runCatching { ZonedDateTime.parse(value.trim(), DateTimeFormatter.RFC_1123_DATE_TIME).toInstant().toEpochMilli() }
    .getOrNull()
