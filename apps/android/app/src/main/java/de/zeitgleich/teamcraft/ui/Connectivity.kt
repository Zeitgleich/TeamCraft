package de.zeitgleich.teamcraft.ui

import android.content.Context
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.platform.LocalContext

/**
 * Whether the device has a validated internet connection, updated live. Messages written while
 * offline wait in the core's outbox; the banner tells the user why nothing new arrives.
 */
@Composable
fun rememberIsOnline(): Boolean {
  val context = LocalContext.current
  val manager = remember(context) { context.getSystemService(Context.CONNECTIVITY_SERVICE) as ConnectivityManager }
  var isOnline by remember { mutableStateOf(manager.isOnline()) }
  DisposableEffect(manager) {
    val callback = object : ConnectivityManager.NetworkCallback() {
      override fun onCapabilitiesChanged(network: Network, capabilities: NetworkCapabilities) {
        isOnline = capabilities.hasCapability(NetworkCapabilities.NET_CAPABILITY_VALIDATED)
      }

      override fun onLost(network: Network) {
        isOnline = false
      }
    }
    manager.registerDefaultNetworkCallback(callback)
    onDispose { manager.unregisterNetworkCallback(callback) }
  }
  return isOnline
}

private fun ConnectivityManager.isOnline(): Boolean =
  getNetworkCapabilities(activeNetwork)?.hasCapability(NetworkCapabilities.NET_CAPABILITY_VALIDATED) == true
