package de.zeitgleich.teamcraft.push

import com.google.firebase.messaging.FirebaseMessagingService
import com.google.firebase.messaging.RemoteMessage
import kotlinx.coroutines.runBlocking
import de.zeitgleich.teamcraft.TeamCraftApplication
import uniffi.teamcraft_ffi.PushService

/** FCM data messages: decrypt and notify in the app (e2ee.md §7, Android row). */
class FcmService : FirebaseMessagingService() {
  override fun onNewToken(token: String) {
    TeamCraftApplication.graph(this).push.onToken(PushService.FCM, token)
  }

  override fun onMessageReceived(message: RemoteMessage) {
    val payload = PushPayload.fromFcmData(message.data) ?: return
    // Called on a background thread; FCM allows about 10 s of work for a high-priority message.
    // Whatever takes longer continues as expedited work.
    // Withdrawals are sent with normal priority and show nothing; the others must show something.
    val isVisible = message.originalPriority == RemoteMessage.PRIORITY_HIGH
    runBlocking { WakeUpWork.handle(this@FcmService, payload, isVisible) }
  }
}
