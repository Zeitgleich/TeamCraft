package de.zeitgleich.teamcraft.auth

import android.net.Uri
import android.util.Log
import de.zeitgleich.teamcraft.core.CoreHttp
import de.zeitgleich.teamcraft.core.CoreHttpConnection
import de.zeitgleich.teamcraft.core.header
import de.zeitgleich.teamcraft.security.DeviceKeyStore
import java.io.IOException
import java.net.HttpURLConnection
import java.net.URL
import java.time.Duration
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.delay
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import net.openid.appauth.AuthorizationServiceConfiguration
import net.openid.appauth.connectivity.ConnectionBuilder
import uniffi.teamcraft_ffi.DeviceProofException
import uniffi.teamcraft_ffi.HttpFetchHeader
import uniffi.teamcraft_ffi.HttpFetchMethod
import uniffi.teamcraft_ffi.HttpFetchRedirects
import uniffi.teamcraft_ffi.HttpFetchRequest
import uniffi.teamcraft_ffi.HttpFetchResponse
import uniffi.teamcraft_ffi.ProviderProver
import uniffi.teamcraft_ffi.TokenEndpointAnswer
import uniffi.teamcraft_ffi.TokenEndpointTransport
import uniffi.teamcraft_ffi.TokenProofFallback

/** One more try for a server that doesn't answer before a sign-in fails for it. */
private const val SERVER_ASK_RETRY_MILLIS = 500L
private const val MAX_SERVER_INFO_BYTES = 1024 * 1024
private val PROBE_CONNECT_TIMEOUT: Duration = Duration.ofSeconds(5)
private val PROBE_READ_TIMEOUT: Duration = Duration.ofSeconds(5)
private const val TAG = "DeviceProofs"
private const val DPOP_HEADER = "DPoP"
private const val DPOP_NONCE_HEADER = "DPoP-Nonce"
/**
 * The decisions about proofs for one server's token requests (docs/e2ee.md §3.1.2, issue #120): the
 * core signs and handles nonce challenges, clock and retries ([ProviderProver]); this says when a
 * proof goes at all, with the rules of the desktop and iOS apps. One per server session, so that the
 * provider's nonce and clock are remembered between refreshes.
 */
class DeviceProofs(
  val keys: DeviceKeyStore,
  private val askServer: suspend () -> ServerProofOffer,
) {
  private val lock = Any()
  private var maybeProver: ProviderProver? = null

  /**
   * The proof of a new sign-in: with one only if the provider takes them and the server would check
   * them. Otherwise, or when no key can be kept, the session stays unbound and nobody is locked out.
   * A server that doesn't answer fails the sign-in, as on the desktop and iOS: it says nothing either
   * way. Reads the Keystore and the network: call it off the main thread.
   */
  suspend fun forSignIn(providerTakesProofs: Boolean): TokenProof {
    if (!providerTakesProofs) return TokenProof.None
    when (askServerPatiently()) {
      ServerProofOffer.OFFERED -> Unit
      ServerProofOffer.NOT_OFFERED -> return TokenProof.None
      ServerProofOffer.NO_ANSWER -> throw IOException("the server doesn't answer whether it checks proofs")
    }
    return newKeyProof(isNewSignIn = true)
  }

  /**
   * The proof of a refresh. A bound session ([maybeBoundKey], the access token's `cnf.jkt`) gets one
   * with its key, whatever discovery says by now, or nothing goes to the provider: a key that is gone
   * or can't be read throws [DeviceProofException]. An unbound one is bound when both sides would
   * do it (sessions from before the app knew DPoP).
   */
  suspend fun forRefresh(maybeBoundKey: String?, providerTakesProofs: Boolean): TokenProof {
    if (maybeBoundKey != null) return TokenProof.With(proverBoundTo(maybeBoundKey), TokenProofFallback.NONE)
    if (knownProver()?.hasRefusedProof() == true) return TokenProof.None
    if (!providerTakesProofs || askServer() != ServerProofOffer.OFFERED) return TokenProof.None
    return newKeyProof(isNewSignIn = false)
  }

  /**
   * A key made to bind an unbound session couldn't sign: it goes, so that the next sign-in makes a
   * new one, and the session stays unbound. Nothing was spent: the signature comes before the request.
   */
  fun discardUnusableKey() {
    Log.w(TAG, "the device key couldn't sign, the session stays unbound")
    deleteKey()
  }

  /** The key goes with its session; nothing learned about the provider is kept for it. */
  fun deleteKey() {
    synchronized(lock) { maybeProver = null }
    keys.delete()
  }

  private fun newKeyProof(isNewSignIn: Boolean): TokenProof =
    try {
      keys.keyOrNew()
      val prover = knownProver()?.takeUnless { isNewSignIn } ?: newProver()
      TokenProof.With(prover, TokenProofFallback.ON_REFUSAL)
    } catch (failure: Exception) {
      Log.w(TAG, "no device key could be kept (${failure.javaClass.simpleName}), the session stays unbound")
      TokenProof.None
    }

  /** The prover of a session bound to [bound]. A key that isn't this one is as lost as none. */
  private fun proverBoundTo(bound: String): ProviderProver {
    knownProver()?.takeIf { it.thumbprint() == bound }?.let { return it }
    val prover = newProver()
    if (prover.thumbprint() != bound) throw DeviceProofException.KeyLost()
    return prover
  }

  private fun knownProver(): ProviderProver? = synchronized(lock) { maybeProver }

  /** Reads the key's public half: throws [DeviceProofException]. */
  private fun newProver(): ProviderProver =
    ProviderProver(keys.proofKey).also { synchronized(lock) { maybeProver = it } }

  private suspend fun askServerPatiently(): ServerProofOffer {
    val first = askServer()
    if (first != ServerProofOffer.NO_ANSWER) return first
    delay(SERVER_ASK_RETRY_MILLIS)
    return askServer()
  }

  companion object {
    /** Whether the provider's discovery document names ES256, the one algorithm the app signs. */
    fun providerTakesProofs(maybeConfiguration: AuthorizationServiceConfiguration?): Boolean {
      val algorithms = maybeConfiguration?.discoveryDoc?.docJson
        ?.optJSONArray("dpop_signing_alg_values_supported") ?: return false
      return (0 until algorithms.length()).any { algorithms.optString(it) == "ES256" }
    }

    /**
     * Asks the server at [serverInfoUrl] whether it would check proofs: a fresh `server-info`
     * (`no-store`), no redirect followed.
     */
    suspend fun askServer(serverInfoUrl: String): ServerProofOffer =
      try {
        val answer = CoreHttp.send(
          CoreHttp.request(
            url = serverInfoUrl,
            headers = listOf(HttpFetchHeader("Cache-Control", "no-store"), HttpFetchHeader("Accept", "application/json")),
            maxBodyBytes = MAX_SERVER_INFO_BYTES,
            connectTimeout = PROBE_CONNECT_TIMEOUT,
            readTimeout = PROBE_READ_TIMEOUT,
          ),
        )
        ServerProofOffer.of(answer.status.toInt(), answer.header(DPOP_NONCE_HEADER))
      } catch (failure: IOException) {
        ServerProofOffer.NO_ANSWER
      }
  }
}

/** What a token request does about DPoP. */
sealed interface TokenProof {
  /** It goes without one: the session isn't bound and nothing says it should be. */
  data object None : TokenProof

  /** With the device's key; [fallback] says when it goes without after all. */
  class With(val prover: ProviderProver, val fallback: TokenProofFallback) : TokenProof
}

/** What a fresh `GET /server-info` says about proofs. */
enum class ServerProofOffer {
  /** It has `DPoP-Nonce`: the server would check them on this host. */
  OFFERED,

  /** It has none, or the server said it has no such page (404) or doesn't know the host (421). */
  NOT_OFFERED,

  /** No answer, or an error status: that says nothing about proofs. */
  NO_ANSWER,
  ;

  companion object {
    fun of(status: Int, maybeNonce: String?): ServerProofOffer = when (status) {
      in 200..299 -> if (maybeNonce == null) NOT_OFFERED else OFFERED
      404, 421 -> NOT_OFFERED
      else -> NO_ANSWER
    }
  }
}

/**
 * AppAuth's requests (discovery, code exchange, token refresh) through the core's HTTP client
 * (issue #433), with AppAuth's own timeouts and without following redirects. While [tokenRequest]
 * runs with a proof, its POST to the token endpoint goes through [ProviderProver.tokenRequest], which
 * signs it and answers the provider's nonce challenge with one more try; AppAuth sees the final answer
 * and checks the tokens as always. HTTPS everywhere; plain HTTP only to the device itself, or anywhere
 * in development builds: the core refuses the rest.
 */
class ProofConnectionBuilder(private val isInsecureHttpAllowed: Boolean) : ConnectionBuilder {
  /** One token request at a time per session, so that a proof goes with the request it was meant for. */
  private val tokenRequests = Mutex()

  @Volatile private var maybeCall: ProofCall? = null

  /**
   * Runs [request], one AppAuth token request, with [proof]. Returns its result and, if the device's
   * key failed (gone, or can't be used now), that failure: AppAuth only sees a network error then.
   */
  suspend fun <T> tokenRequest(proof: TokenProof, request: suspend () -> T): Pair<T, DeviceProofException?> =
    tokenRequests.withLock {
      val maybeProofCall = (proof as? TokenProof.With)?.let { ProofCall(it.prover, it.fallback) }
      maybeCall = maybeProofCall
      try {
        request() to maybeProofCall?.maybeKeyFailure
      } finally {
        maybeCall = null
      }
    }

  override fun openConnection(uri: Uri): HttpURLConnection {
    // Taken when AppAuth opens the connection, so the request keeps the proof it started with.
    val maybeProofCall = maybeCall
    return CoreHttpConnection(URL(uri.toString()), isInsecureHttpAllowed, transport = { request ->
      if (maybeProofCall != null && request.method == HttpFetchMethod.POST) {
        maybeProofCall.send(request)
      } else {
        CoreHttp.send(request)
      }
    }).apply {
      connectTimeout = TimeUnit.SECONDS.toMillis(15).toInt()
      readTimeout = TimeUnit.SECONDS.toMillis(10).toInt()
      instanceFollowRedirects = false
    }
  }
}

/** One token request with the device's proof, as AppAuth wrote it. */
private class ProofCall(private val prover: ProviderProver, private val fallback: TokenProofFallback) {
  @Volatile var maybeKeyFailure: DeviceProofException? = null

  /** The final answer; a failure is an [IOException], the only kind AppAuth handles. */
  suspend fun send(request: HttpFetchRequest): HttpFetchResponse {
    val transport = Transport(request)
    try {
      prover.tokenRequest(request.url, fallback, transport)
    } catch (noAnswer: DeviceProofException.NoAnswer) {
      throw transport.maybeFailure ?: IOException("no answer from the token endpoint")
    } catch (keyFailure: DeviceProofException) {
      maybeKeyFailure = keyFailure
      throw IOException("the device key can't sign", keyFailure)
    }
    return transport.maybeLastAnswer ?: throw IOException("no answer from the token endpoint")
  }

  /**
   * Sends the request with the proof the core made, or without one, and keeps the raw answer for
   * AppAuth. With a proof it follows no redirect: the proof names the first URL.
   */
  private class Transport(private val request: HttpFetchRequest) : TokenEndpointTransport {
    @Volatile var maybeLastAnswer: HttpFetchResponse? = null
    @Volatile var maybeFailure: IOException? = null

    override suspend fun send(proof: String?): TokenEndpointAnswer {
      val headers = request.headers.filterNot { it.name.equals(DPOP_HEADER, ignoreCase = true) } +
        listOfNotNull(proof?.let { HttpFetchHeader(DPOP_HEADER, it) })
      val answer = try {
        CoreHttp.send(request.copy(headers = headers, redirects = HttpFetchRedirects.DONT_FOLLOW))
      } catch (failure: IOException) {
        maybeFailure = failure
        throw DeviceProofException.NoAnswer(failure.javaClass.simpleName)
      }
      maybeLastAnswer = answer
      return TokenEndpointAnswer(
        status = answer.status,
        body = answer.body,
        nonce = answer.header(DPOP_NONCE_HEADER),
        date = answer.header("Date"),
      )
    }
  }
}
