package de.zeitgleich.teamcraft.core

import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream
import java.io.FileNotFoundException
import java.io.IOException
import java.io.InputStream
import java.io.OutputStream
import java.net.HttpURLConnection
import java.net.ProtocolException
import java.net.URL
import java.time.Duration
import java.util.TreeMap
import kotlinx.coroutines.runBlocking
import uniffi.teamcraft_ffi.HttpFetchHeader
import uniffi.teamcraft_ffi.HttpFetchMethod
import uniffi.teamcraft_ffi.HttpFetchRedirects
import uniffi.teamcraft_ffi.HttpFetchRequest
import uniffi.teamcraft_ffi.HttpFetchResponse

/** Largest answer read; AppAuth's (the discovery document, tokens) are a few kilobytes. */
private const val MAX_ANSWER_BYTES = 1024 * 1024

/** What the platform's "no timeout" (0) becomes: the core always has one. */
private val FALLBACK_TIMEOUT: Duration = Duration.ofSeconds(30)

/**
 * An [HttpURLConnection] whose request goes through the core's HTTP client ([CoreHttp]), for
 * AppAuth: its `ConnectionBuilder` has to hand out `HttpURLConnection`s (discovery, code exchange,
 * token refresh), and the platform's waits out the connect timeout on a black-holed IPv6 route
 * (issue #433). The request body is kept in memory and the request sent when the answer is first
 * asked for, blocking the calling thread (AppAuth's background executor) as the platform's does.
 * Every failure is an [IOException]: AppAuth handles nothing else.
 */
class CoreHttpConnection(
  url: URL,
  private val isInsecureHttpAllowed: Boolean,
  private val transport: suspend (HttpFetchRequest) -> HttpFetchResponse = { CoreHttp.send(it) },
) : HttpURLConnection(url) {
  /** Case-insensitive, as the platform's. */
  private val requestProperties = TreeMap<String, MutableList<String>>(String.CASE_INSENSITIVE_ORDER)
  private var maybeBody: ByteArrayOutputStream? = null
  private var maybeAnswer: HttpFetchResponse? = null
  private var maybeFailure: IOException? = null

  /** Sends the request, unless its body is still to be written ([getOutputStream]). */
  override fun connect() {
    if (!getDoOutput()) answer()
  }

  /** Nothing to close: the core has read the whole answer and keeps its connections itself. */
  override fun disconnect() = Unit

  /** The core picks the proxy; this connection doesn't learn whether it used one. */
  override fun usingProxy(): Boolean = false

  override fun setRequestProperty(key: String, value: String?) {
    checkNotSent()
    requestProperties[key] = listOfNotNull(value).toMutableList()
  }

  override fun addRequestProperty(key: String, value: String?) {
    checkNotSent()
    val values = requestProperties.getOrPut(key) { mutableListOf() }
    if (value != null) values.add(value)
  }

  override fun getRequestProperty(key: String?): String? = key?.let { requestProperties[it]?.firstOrNull() }

  override fun getRequestProperties(): Map<String, List<String>> = requestProperties.mapValues { it.value.toList() }

  override fun getOutputStream(): OutputStream {
    if (!getDoOutput()) throw ProtocolException("this request has no body (doOutput is false)")
    if (connected) throw ProtocolException("the request was already sent")
    return maybeBody ?: ByteArrayOutputStream().also { maybeBody = it }
  }

  override fun getResponseCode(): Int = answer().status.toInt()

  /** The body of a successful answer; an error answer's is on [getErrorStream], as the platform's. */
  override fun getInputStream(): InputStream {
    val answer = answer()
    val status = answer.status.toInt()
    if (status == HttpURLConnection.HTTP_NOT_FOUND || status == HttpURLConnection.HTTP_GONE) {
      throw FileNotFoundException(getURL().toString())
    }
    if (status >= HttpURLConnection.HTTP_BAD_REQUEST) throw IOException("HTTP $status for ${getURL()}")
    return ByteArrayInputStream(answer.body)
  }

  override fun getErrorStream(): InputStream? {
    val answer = maybeAnswer ?: return null
    val isError = answer.status.toInt() >= HttpURLConnection.HTTP_BAD_REQUEST
    return if (isError && answer.body.isNotEmpty()) ByteArrayInputStream(answer.body) else null
  }

  override fun getHeaderField(name: String?): String? = name?.let { quietAnswer()?.header(it) }

  override fun getHeaderFields(): Map<String, List<String>> =
    quietAnswer()?.headers?.groupBy({ it.name }, { it.value }).orEmpty()

  /** Index 0 is the status line, which has no name. */
  override fun getHeaderFieldKey(n: Int): String? = if (n == 0) null else quietAnswer()?.headers?.getOrNull(n - 1)?.name

  override fun getHeaderField(n: Int): String? {
    val answer = quietAnswer() ?: return null
    return if (n == 0) "HTTP/1.1 ${answer.status}" else answer.headers.getOrNull(n - 1)?.value
  }

  /** The answer, asked for once; a failure is kept and thrown again. */
  private fun answer(): HttpFetchResponse {
    maybeAnswer?.let { return it }
    maybeFailure?.let { throw it }
    try {
      val request = request()
      connected = true
      val answer = runBlocking { transport(request) }
      maybeAnswer = answer
      return answer
    } catch (failure: IOException) {
      maybeFailure = failure
      throw failure
    } catch (failure: Exception) {
      // AppAuth catches IOExceptions only; anything else would end its background task with a crash.
      val wrapped = IOException(failure)
      maybeFailure = wrapped
      throw wrapped
    }
  }

  /** The platform's header accessors answer null instead of throwing. */
  private fun quietAnswer(): HttpFetchResponse? =
    try {
      answer()
    } catch (failure: IOException) {
      null
    }

  private fun request(): HttpFetchRequest {
    val maybeWritten = maybeBody?.toByteArray()
    val method = when (getRequestMethod()) {
      "POST" -> HttpFetchMethod.POST
      // As the platform's: a GET with a body goes out as a POST.
      "GET" -> if (maybeWritten != null) HttpFetchMethod.POST else HttpFetchMethod.GET
      else -> throw ProtocolException("method ${getRequestMethod()} is not supported")
    }
    return HttpFetchRequest(
      method = method,
      url = getURL().toString(),
      headers = requestProperties.flatMap { (name, values) -> values.map { HttpFetchHeader(name, it) } },
      body = maybeWritten ?: ByteArray(0),
      redirects = if (getInstanceFollowRedirects()) HttpFetchRedirects.FOLLOW else HttpFetchRedirects.DONT_FOLLOW,
      allowInsecureHttp = isInsecureHttpAllowed,
      connectTimeout = timeout(getConnectTimeout()),
      readTimeout = timeout(getReadTimeout()),
      maxBodyBytes = MAX_ANSWER_BYTES.toULong(),
    )
  }

  private fun checkNotSent() = check(!connected) { "the request was already sent" }

  private fun timeout(millis: Int): Duration = if (millis > 0) Duration.ofMillis(millis.toLong()) else FALLBACK_TIMEOUT
}
