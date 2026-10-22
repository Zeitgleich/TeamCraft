package de.zeitgleich.teamcraft.push

import android.content.Context
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import de.zeitgleich.teamcraft.TeamCraftApplication
import org.unifiedpush.android.connector.FailedReason
import org.unifiedpush.android.connector.MessagingReceiver
import org.unifiedpush.android.connector.data.PushEndpoint
import org.unifiedpush.android.connector.data.PushMessage
import uniffi.teamcraft_ffi.PushService

/**
 * UnifiedPush endpoints and messages. The connector removes the Web Push encryption (RFC 8291);
 * what remains is the sealed wake-up.
 */
class UnifiedPushReceiver : MessagingReceiver() {
  override fun onNewEndpoint(context: Context, endpoint: PushEndpoint, instance: String) {
    // A temporary fallback endpoint is registered too: it is better than no notifications.
    val keys = endpoint.pubKeySet
    val token = PushSubscription.json(endpoint.url, keys?.pubKey, keys?.auth)
    TeamCraftApplication.graph(context).push.onToken(PushService.UNIFIED_PUSH, token)
  }

  override fun onMessage(context: Context, message: PushMessage, instance: String) {
    val pending = goAsync()
    CoroutineScope(Dispatchers.Default).launch {
      try {
        TeamCraftApplication.graph(context).push.onWakeUp(message.content)
      } finally {
        pending.finish()
      }
    }
  }

  override fun onRegistrationFailed(context: Context, reason: FailedReason, instance: String) {}

  override fun onUnregistered(context: Context, instance: String) {}
}
