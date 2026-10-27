package de.zeitgleich.teamcraft.core

import java.io.FileNotFoundException
import java.io.IOException
import java.io.OutputStreamWriter
import java.net.MalformedURLException
import java.net.ProtocolException
import java.net.SocketTimeoutException
import java.net.URL
import java.net.UnknownServiceException
import java.time.Duration
import java.time.Instant
import javax.net.ssl.SSLHandshakeException
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.server.ResponseTooLargeException
import de.zeitgleich.teamcraft.server.transportErrorText
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test
import uniffi.teamcraft_ffi.HttpFetchException
import uniffi.teamcraft_ffi.HttpFetchHeader
import uniffi.teamcraft_ffi.HttpFetchMethod
import uniffi.teamcraft_ffi.HttpFetchRedirects
import uniffi.teamcraft_ffi.HttpFetchRequest
import uniffi.teamcraft_ffi.HttpFetchResponse

private const val TOKEN_URL = "https://idp.example.com/realms/team/protocol/openid-connect/token"
private const val DISCOVERY_URL = "https://idp.example.com/realms/team/.well-known/openid-configuration"
private const val FORM = "grant_type=refresh_token&refresh_token=r1&client_id=teamcraft"
private const val DATE = "Tue, 27 Oct 2026 10:00:00 GMT"

class CoreHttpTest {
  /** AppAuth's requests, made the way AppAuth makes them, through [CoreHttpConnection]. */
  class AppAuthConnections {
    @Test
    fun aTokenRequestGoesOutAsOnePostWithItsForm() {
      val transport = FakeTransport(answer(200, """{"access_token":"a"}"""))
      val connection = appAuthConnection(TOKEN_URL, transport)

      // AuthorizationService.TokenRequestTask
      connection.requestMethod = "POST"
      connection.setRequestProperty("Content-Type", "application/x-www-form-urlencoded")
      if (connection.getRequestProperty("Accept").isNullOrEmpty()) connection.setRequestProperty("Accept", "application/json")
      connection.doOutput = true
      connection.setRequestProperty("Content-Length", FORM.length.toString())
      OutputStreamWriter(connection.outputStream).apply {
        write(FORM)
        flush()
      }
      val status = connection.responseCode
      val body = (if (status in 200..299) connection.inputStream else connection.errorStream!!).bufferedReader().readText()

      assertEquals(200, status)
      assertEquals("""{"access_token":"a"}""", body)
      val sent = transport.sent.single()
      assertEquals(HttpFetchMethod.POST, sent.method)
      assertEquals(TOKEN_URL, sent.url)
      assertEquals(
        listOf(
          HttpFetchHeader("Accept", "application/json"),
          HttpFetchHeader("Content-Length", FORM.length.toString()),
          HttpFetchHeader("Content-Type", "application/x-www-form-urlencoded"),
        ),
        sent.headers.sortedBy { it.name },
      )
      assertArrayEquals(FORM.encodeToByteArray(), sent.body)
      assertEquals(HttpFetchRedirects.DONT_FOLLOW, sent.redirects)
      assertFalse(sent.allowInsecureHttp)
      assertEquals(Duration.ofSeconds(15), sent.connectTimeout)
      assertEquals(Duration.ofSeconds(10), sent.readTimeout)
    }

    /** A refused refresh token: AppAuth reads `invalid_grant` from the error stream. */
    @Test
    fun anErrorAnswerIsReadFromTheErrorStream() {
      val connection = appAuthConnection(TOKEN_URL, FakeTransport(answer(400, """{"error":"invalid_grant"}""")))
      connection.requestMethod = "POST"
      connection.doOutput = true
      connection.outputStream.write(FORM.encodeToByteArray())

      assertEquals(400, connection.responseCode)
      assertEquals("""{"error":"invalid_grant"}""", connection.errorStream!!.bufferedReader().readText())
      assertThrows<IOException> { connection.inputStream }
    }

    @Test
    fun discoveryReadsTheDocumentAfterConnect() {
      val transport = FakeTransport(answer(200, """{"issuer":"https://idp.example.com/realms/team"}"""))
      val connection = appAuthConnection(DISCOVERY_URL, transport)

      // AuthorizationServiceConfiguration.ConfigurationRetrievalAsyncTask
      connection.requestMethod = "GET"
      connection.doInput = true
      connection.connect()
      val document = connection.inputStream.bufferedReader().readText()

      assertEquals("""{"issuer":"https://idp.example.com/realms/team"}""", document)
      val sent = transport.sent.single()
      assertEquals(HttpFetchMethod.GET, sent.method)
      assertEquals(0, sent.body.size)
      assertNull(connection.errorStream)
    }

    @Test
    fun aMissingDocumentIsAnIoException() {
      val connection = appAuthConnection(DISCOVERY_URL, FakeTransport(answer(404, "")))
      connection.connect()
      assertThrows<FileNotFoundException> { connection.inputStream }
      assertNull("an empty error answer has no stream", connection.errorStream)
    }

    /** AppAuth catches only IOExceptions; anything else would crash its background task. */
    @Test
    fun everyFailureIsAnIoExceptionAndTheRequestGoesOutOnce() {
      val transport = FakeTransport(failure = IllegalStateException("no runtime"))
      val connection = appAuthConnection(DISCOVERY_URL, transport)

      val first = assertThrows<IOException> { connection.connect() }
      val again = assertThrows<IOException> { connection.inputStream }

      assertSame(first, again)
      assertEquals(1, transport.sent.size)
      assertNull(connection.getHeaderField("Date"))
    }

    @Test
    fun aTransportFailureKeepsItsKind() {
      val connection = appAuthConnection(DISCOVERY_URL, FakeTransport(failure = SocketTimeoutException("silent")))
      assertThrows<SocketTimeoutException> { connection.responseCode }
    }

    @Test
    fun nothingChangesOnceTheRequestIsSent() {
      val connection = appAuthConnection(TOKEN_URL, FakeTransport(answer(200, "{}")))
      connection.doOutput = true
      connection.outputStream.write(FORM.encodeToByteArray())
      connection.responseCode

      assertThrows<ProtocolException> { connection.outputStream }
      assertThrows<IllegalStateException> { connection.setRequestProperty("Accept", "text/plain") }
    }

    @Test
    fun answerHeadersAreFoundInAnyCase() {
      val headers = listOf(HttpFetchHeader("content-type", "application/json"), HttpFetchHeader("date", DATE))
      val connection = appAuthConnection(DISCOVERY_URL, FakeTransport(answer(200, "{}", headers)))

      assertEquals("application/json", connection.contentType)
      assertEquals("application/json", connection.getHeaderField("Content-Type"))
      assertEquals(httpDateMillis(DATE), connection.getHeaderFieldDate("Date", 0L))
      assertEquals("HTTP/1.1 200", connection.getHeaderField(0))
      assertNull(connection.getHeaderFieldKey(0))
      assertEquals("date", connection.getHeaderFieldKey(2))
      assertEquals(listOf(DATE), connection.headerFields["date"])
    }

    @Test
    fun aGetWithABodyGoesOutAsAPostAndOtherMethodsAreRefused() {
      val transport = FakeTransport(answer(200, "{}"))
      val withBody = appAuthConnection(TOKEN_URL, transport)
      withBody.doOutput = true
      withBody.outputStream.write(FORM.encodeToByteArray())
      withBody.responseCode
      assertEquals(HttpFetchMethod.POST, transport.sent.single().method)

      val delete = appAuthConnection(TOKEN_URL, transport)
      delete.requestMethod = "DELETE"
      assertThrows<ProtocolException> { delete.responseCode }
      assertEquals(1, transport.sent.size)
    }

    @Test
    fun followedRedirectsPlainHttpAndNoTimeoutAreSaidToTheCore() {
      val transport = FakeTransport(answer(200, "{}"))
      val connection = CoreHttpConnection(URL("http://10.0.2.2:8080/realms/team/token"), isInsecureHttpAllowed = true, transport::send)
      connection.instanceFollowRedirects = true
      connection.responseCode

      val sent = transport.sent.single()
      assertEquals(HttpFetchRedirects.FOLLOW, sent.redirects)
      assertTrue(sent.allowInsecureHttp)
      assertTrue(sent.connectTimeout > Duration.ZERO && sent.readTimeout > Duration.ZERO)
    }
  }

  class Failures {
    @Test
    fun eachFailureBecomesTheExceptionThePlatformThrowsForIt() {
      val cases = listOf(
        HttpFetchException.InvalidRequest("x") to MalformedURLException::class.java,
        HttpFetchException.PlainHttpRefused("x") to UnknownServiceException::class.java,
        HttpFetchException.UntrustedCertificate("x") to SSLHandshakeException::class.java,
        HttpFetchException.TimedOut("x") to SocketTimeoutException::class.java,
        HttpFetchException.TooLarge("x") to ResponseTooLargeException::class.java,
        HttpFetchException.Network("x") to IOException::class.java,
      )
      for ((failure, type) in cases) {
        val mapped = failure.asIoException()
        assertEquals(type, mapped.javaClass)
        assertSame(failure, mapped.cause)
      }
    }

    /** The server entry tells the user what to do about each. */
    @Test
    fun theServerEntryStillTellsThemApart() {
      fun text(failure: HttpFetchException, isOnline: Boolean = true) = transportErrorText(failure.asIoException(), isOnline)
      assertEquals(R.string.server_error_https_only, text(HttpFetchException.PlainHttpRefused("chat.example.com")))
      assertEquals(R.string.server_error_tls, text(HttpFetchException.UntrustedCertificate("unknown issuer")))
      assertEquals(R.string.server_error_tls, text(HttpFetchException.UntrustedCertificate("unknown issuer"), isOnline = false))
      assertEquals(R.string.server_error_not_teamcraft, text(HttpFetchException.TooLarge("1 MiB")))
      assertEquals(R.string.server_error_unreachable, text(HttpFetchException.TimedOut("connect")))
      assertEquals(R.string.server_error_offline, text(HttpFetchException.Network("refused"), isOnline = false))
    }
  }

  class Answers {
    @Test
    fun headersAreLookedUpInAnyCaseAndTheLastOneCounts() {
      val answer = answer(200, "", listOf(HttpFetchHeader("set-cookie", "a"), HttpFetchHeader("set-cookie", "b")))
      assertEquals("b", answer.header("Set-Cookie"))
      assertNull(answer.header("Date"))
    }

    @Test
    fun readsTheServersDate() {
      assertEquals(Instant.parse("2026-10-27T10:00:00Z").toEpochMilli(), httpDateMillis(DATE))
      assertEquals(Instant.parse("2026-10-27T10:00:00Z").toEpochMilli(), httpDateMillis(" $DATE "))
      assertNull(httpDateMillis("yesterday"))
      assertNull(httpDateMillis(""))
    }
  }
}

/** Answers every request with [answer], or fails with [failure]; keeps what was sent. */
private class FakeTransport(
  private val answer: HttpFetchResponse? = null,
  private val failure: Exception? = null,
) {
  val sent = mutableListOf<HttpFetchRequest>()

  @Suppress("RedundantSuspendModifier")
  suspend fun send(request: HttpFetchRequest): HttpFetchResponse {
    sent += request
    failure?.let { throw it }
    return answer ?: throw IllegalStateException("no answer")
  }
}

/** A connection as AuthRepository's `CoreConnectionBuilder` opens it. */
private fun appAuthConnection(url: String, transport: FakeTransport) =
  CoreHttpConnection(URL(url), isInsecureHttpAllowed = false, transport::send).apply {
    connectTimeout = 15_000
    readTimeout = 10_000
    instanceFollowRedirects = false
  }

private fun answer(status: Int, body: String, headers: List<HttpFetchHeader> = emptyList()) =
  HttpFetchResponse(status = status.toUShort(), headers = headers, body = body.encodeToByteArray())

private inline fun <reified T : Throwable> assertThrows(block: () -> Unit): T {
  try {
    block()
  } catch (thrown: Throwable) {
    if (thrown is T) return thrown
    throw AssertionError("expected ${T::class.java.simpleName}, got $thrown", thrown)
  }
  fail("expected ${T::class.java.simpleName}")
  throw AssertionError()
}
