package de.zeitgleich.teamcraft.push

import android.content.Context
import org.unifiedpush.android.connector.UnifiedPush
import uniffi.teamcraft_ffi.PushService

/** UnifiedPush through whatever distributor the user installed (ntfy, NextPush, ...). */
object UnifiedPushBackend : PushBackend {
  override val service = PushService.UNIFIED_PUSH

  override fun isAvailable(context: Context): Boolean =
    UnifiedPush.getDistributors(context).isNotEmpty()

  override fun register(context: Context) {
    if (UnifiedPush.getAckDistributor(context) == null) {
      UnifiedPush.getDistributors(context).firstOrNull()?.let { UnifiedPush.saveDistributor(context, it) }
    }
    UnifiedPush.register(context)
  }

  override fun unregister(context: Context) {
    if (UnifiedPush.getAckDistributor(context) != null) UnifiedPush.unregister(context)
  }
}
