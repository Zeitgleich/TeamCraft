package de.zeitgleich.teamcraft.push

import android.content.Context
import com.google.firebase.FirebaseApp
import com.google.firebase.FirebaseOptions
import com.google.firebase.messaging.FirebaseMessaging
import de.zeitgleich.teamcraft.BuildConfig
import de.zeitgleich.teamcraft.TeamCraftApplication
import uniffi.teamcraft_ffi.PushService

/**
 * FCM, configured from build settings (no google-services.json, so each deployment brings its own
 * Firebase project). Messages are data-only and carry only the sealed wake-up.
 */
object FcmBackend : PushBackend {
  override val service = PushService.FCM

  override fun isAvailable(context: Context): Boolean =
    BuildConfig.FCM_PROJECT_ID.isNotEmpty() && BuildConfig.FCM_APPLICATION_ID.isNotEmpty()

  override fun register(context: Context) {
    ensureInitialized(context)
    FirebaseMessaging.getInstance().isAutoInitEnabled = true
    FirebaseMessaging.getInstance().token.addOnSuccessListener { token ->
      TeamCraftApplication.graph(context).push.onToken(PushService.FCM, token)
    }
  }

  fun ensureInitialized(context: Context) {
    if (FirebaseApp.getApps(context).isNotEmpty()) return
    val options = FirebaseOptions.Builder()
      .setProjectId(BuildConfig.FCM_PROJECT_ID)
      .setApplicationId(BuildConfig.FCM_APPLICATION_ID)
      .setApiKey(BuildConfig.FCM_API_KEY)
      .setGcmSenderId(BuildConfig.FCM_SENDER_ID)
      .build()
    FirebaseApp.initializeApp(context, options)
  }
}
