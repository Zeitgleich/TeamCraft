package de.zeitgleich.teamcraft.core

import android.content.Context
import android.content.RestrictionsManager
import android.net.LinkProperties
import android.net.Uri
import android.util.Log
import java.security.KeyStore
import java.security.cert.X509Certificate
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import uniffi.teamcraft_ffi.NetworkProxy
import uniffi.teamcraft_ffi.setNetworkProxy
import uniffi.teamcraft_ffi.setTrustedCertificates

/** Key of `res/xml/app_restrictions.xml`; part of the contract with the organization's MDM. */
const val RESTRICTION_TRUST_USER_CERTIFICATES = "trust_user_certificates"

private const val TAG = "NetworkTrust"
private const val SYSTEM_ALIAS_PREFIX = "system:"
private const val USER_ALIAS_PREFIX = "user:"

/**
 * Hands the device's network setup to the Rust core, which can't read it itself (issue #157):
 * the certificate authorities it trusts and the HTTP proxy of the current network.
 */
object NetworkTrust {
  /**
   * The CAs the core trusts, from `AndroidCAStore`: the system CAs as the user left them enabled,
   * and CAs a user or the device management installed only when the organization's MDM turns on
   * [RESTRICTION_TRUST_USER_CERTIFICATES] (a TLS-inspecting company proxy). Android apps ignore user
   * CAs by default, so a CA someone talked the user into installing reads nothing. Must run before
   * the first core connection; the core keeps the first list it gets.
   */
  fun installTrustedCertificates(context: Context) {
    val isUserTrusted = runCatching {
      val manager = context.getSystemService(Context.RESTRICTIONS_SERVICE) as? RestrictionsManager
      manager?.applicationRestrictions?.getBoolean(RESTRICTION_TRUST_USER_CERTIFICATES, false) == true
    }.getOrDefault(false)
    val certificates = runCatching {
      val store = KeyStore.getInstance("AndroidCAStore").apply { load(null) }
      store.aliases().toList()
        .filter { it.startsWith(SYSTEM_ALIAS_PREFIX) || (isUserTrusted && it.startsWith(USER_ALIAS_PREFIX)) }
        .mapNotNull { (store.getCertificate(it) as? X509Certificate)?.encoded }
    }.getOrElse { failure ->
      // The core then trusts the Mozilla roots it ships with.
      Log.w(TAG, "system CAs not readable: ${failure.javaClass.simpleName}")
      emptyList()
    }
    setTrustedCertificates(certificates)
  }

  @Volatile private var maybeReported: NetworkProxy? = null
  @Volatile private var hasReported = false
  private val installed = CompletableDeferred<Unit>()

  /**
   * Reads the CAs off the main thread ([installTrustedCertificates]); the core waits for it with
   * [awaitTrustedCertificates] before it opens.
   */
  fun startInstalling(context: Context, scope: CoroutineScope) {
    scope.launch(Dispatchers.IO) {
      try {
        installTrustedCertificates(context)
      } finally {
        installed.complete(Unit)
      }
    }
  }

  suspend fun awaitTrustedCertificates() = installed.await()

  /**
   * The proxy of the network with [properties] (`null`: none known) for every core connection.
   * Returns whether it differs from the one reported before.
   */
  @Synchronized
  fun reportProxy(properties: LinkProperties?): Boolean {
    val maybeProxy = proxyOf(properties)
    val isChanged = !hasReported || maybeProxy != maybeReported
    hasReported = true
    maybeReported = maybeProxy
    if (isChanged) setNetworkProxy(maybeProxy)
    return isChanged
  }

  private fun proxyOf(properties: LinkProperties?): NetworkProxy? {
    val proxy = properties?.httpProxy ?: return null
    val host = proxy.host
    if (host.isNullOrEmpty() || proxy.port <= 0) {
      if (proxy.pacFileUrl != Uri.EMPTY) {
        Log.w(TAG, "the network uses a PAC script, which the core doesn't evaluate; connecting directly")
      }
      return null
    }
    return NetworkProxy(
      host = host,
      port = proxy.port.toUShort(),
      exclusions = proxy.exclusionList?.toList().orEmpty(),
    )
  }
}
