package de.zeitgleich.teamcraft.ui

import android.content.Context
import android.net.ConnectivityManager
import android.net.LinkProperties
import android.net.Network
import android.net.NetworkCapabilities
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.platform.LocalContext
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import de.zeitgleich.teamcraft.TeamCraftApplication
import de.zeitgleich.teamcraft.core.NetworkTrust

/**
 * Follows the default network for the whole process. Every switch (Wi-Fi to mobile, a VPN coming
 * up, airplane mode off) is reported to the core through [onChange], which checks its gateway
 * connection at once instead of waiting for its own detection and backoff. [isOnline] says whether
 * the device has a validated internet connection at all; whether the server is reachable is the
 * core's connection state. The network's HTTP proxy goes to the core as well (issue #157).
 */
class NetworkWatcher(context: Context, private val onChange: () -> Unit) {
  private val manager = context.getSystemService(Context.CONNECTIVITY_SERVICE) as ConnectivityManager
  private val online = MutableStateFlow(manager.isOnline())
  private var maybeNetwork: Network? = null
  private var wasValidated = online.value

  val isOnline: StateFlow<Boolean> = online

  fun start() {
    NetworkTrust.reportProxy(manager.getLinkProperties(manager.activeNetwork))
    manager.registerDefaultNetworkCallback(
      object : ConnectivityManager.NetworkCallback() {
        override fun onAvailable(network: Network) = changed(network, isValidated = wasValidated)

        override fun onCapabilitiesChanged(network: Network, capabilities: NetworkCapabilities) =
          changed(network, capabilities.hasCapability(NetworkCapabilities.NET_CAPABILITY_VALIDATED))

        override fun onLinkPropertiesChanged(network: Network, properties: LinkProperties) {
          // Before the reconnect that a new network triggers, so it goes through the right proxy.
          NetworkTrust.reportProxy(properties)
          changed(network, isValidated = wasValidated)
        }

        override fun onLost(network: Network) = changed(null, isValidated = false)
      },
    )
  }

  /** Capabilities change often (signal strength); only another network or validation counts. */
  @Synchronized
  private fun changed(maybeNow: Network?, isValidated: Boolean) {
    val isNew = maybeNow != maybeNetwork || isValidated != wasValidated
    maybeNetwork = maybeNow
    wasValidated = isValidated
    online.value = isValidated
    if (isNew) onChange()
  }
}

/** Whether the device has a validated internet connection, updated live. */
@Composable
fun rememberIsOnline(): Boolean {
  val graph = TeamCraftApplication.graph(LocalContext.current)
  val isOnline by graph.network.isOnline.collectAsState()
  return isOnline
}

private fun ConnectivityManager.isOnline(): Boolean =
  getNetworkCapabilities(activeNetwork)?.hasCapability(NetworkCapabilities.NET_CAPABILITY_VALIDATED) == true
