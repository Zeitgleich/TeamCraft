package de.zeitgleich.teamcraft.calls

import android.content.Context
import android.net.ConnectivityManager
import android.net.LinkProperties
import android.net.Network
import android.util.Log

/**
 * Tells the call engine when the phone's default network changes (Wi-Fi to mobile data and back,
 * a lost network, new addresses): the engine then checks that the call's connection followed and
 * rejoins at once otherwise (ARCHITECTURE.md §6.5). Callbacks arrive on the connectivity thread.
 */
internal class NetworkWatcher(context: Context, private val onChange: () -> Unit) {
  private val connectivity = context.getSystemService(ConnectivityManager::class.java)
  @Volatile private var maybeCurrent: Network? = null
  @Volatile private var maybeAddresses: Set<String>? = null
  @Volatile private var isFirst = true

  private val callback = object : ConnectivityManager.NetworkCallback() {
    override fun onAvailable(network: Network) {
      // The first callback reports the network the call started on.
      val isSwitch = !isFirst && network != maybeCurrent
      isFirst = false
      maybeCurrent = network
      maybeAddresses = null
      if (isSwitch) onChange()
    }

    override fun onLost(network: Network) {
      if (network != maybeCurrent) return
      maybeCurrent = null
      onChange()
    }

    override fun onLinkPropertiesChanged(network: Network, linkProperties: LinkProperties) {
      if (network != maybeCurrent) return
      val addresses = linkProperties.linkAddresses.map { it.address.hostAddress.orEmpty() }.toSet()
      val maybePrevious = maybeAddresses
      maybeAddresses = addresses
      if (maybePrevious != null && maybePrevious != addresses) onChange()
    }
  }

  fun start() {
    try {
      connectivity.registerDefaultNetworkCallback(callback)
    } catch (error: RuntimeException) {
      // Too many callbacks registered: the call still notices a dead path by itself, only later.
      Log.w(TAG, "watching the network failed", error)
    }
  }

  fun stop() {
    try {
      connectivity.unregisterNetworkCallback(callback)
    } catch (error: IllegalArgumentException) {
      // Never registered (start failed).
    }
  }

  private companion object {
    const val TAG = "TeamCraftNetwork"
  }
}
