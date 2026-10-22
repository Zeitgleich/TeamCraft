package de.zeitgleich.teamcraft.push

import android.content.Context
import uniffi.teamcraft_ffi.PushService

/** A way to be woken up while the app is closed (FCM, UnifiedPush). Vendor-neutral by design. */
interface PushBackend {
  val service: PushService

  /** Whether this backend can work on this device (configured, distributor installed, ...). */
  fun isAvailable(context: Context): Boolean

  /** Starts registration; the token arrives later through [PushManager.onToken]. */
  fun register(context: Context)
}
