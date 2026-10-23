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
    UnifiedPushStatus(context).record(UnifiedPushStatus.State.REGISTERED)
    TeamCraftApplication.graph(context).push.onToken(PushService.UNIFIED_PUSH, token)
  }

  override fun onMessage(context: Context, message: PushMessage, instance: String) {
    val pending = goAsync()
    CoroutineScope(Dispatchers.Default).launch {
      try {
        WakeUpWork.handle(context, message.content)
      } finally {
        pending.finish()
      }
    }
  }

  // The diagnostics show these; registering again is the user's choice there.
  override fun onRegistrationFailed(context: Context, reason: FailedReason, instance: String) {
    UnifiedPushStatus(context).record(UnifiedPushStatus.State.FAILED, reason.name)
  }

  override fun onUnregistered(context: Context, instance: String) {
    UnifiedPushStatus(context).record(UnifiedPushStatus.State.UNREGISTERED)
  }

  override fun onTempUnavailable(context: Context, instance: String) {
    UnifiedPushStatus(context).record(UnifiedPushStatus.State.TEMPORARILY_UNAVAILABLE)
  }
}
