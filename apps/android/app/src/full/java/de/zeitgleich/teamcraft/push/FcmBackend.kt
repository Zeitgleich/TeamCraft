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
    val messaging = FirebaseMessaging.getInstance()
    messaging.isAutoInitEnabled = true
    val prefs = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
    if (!prefs.getBoolean(KEY_ROTATED_FOR_SEALING, false)) {
      // Once, when the app starts sealing its token: versions before handed the raw token to
      // servers, which could seal it for themselves forever. A fresh token makes those copies dead.
      messaging.deleteToken().addOnCompleteListener {
        prefs.edit().putBoolean(KEY_ROTATED_FOR_SEALING, true).apply()
        requestToken(context, messaging)
      }
      return
    }
    requestToken(context, messaging)
  }

  private fun requestToken(context: Context, messaging: FirebaseMessaging) {
    messaging.token.addOnSuccessListener { token ->
      TeamCraftApplication.graph(context).push.onToken(PushService.FCM, token)
    }
  }

  private const val PREFS = "fcm"
  private const val KEY_ROTATED_FOR_SEALING = "rotated_for_sealing"

  override fun unregister(context: Context) {
    if (!isAvailable(context)) return
    ensureInitialized(context)
    FirebaseMessaging.getInstance().isAutoInitEnabled = false
    FirebaseMessaging.getInstance().deleteToken()
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
