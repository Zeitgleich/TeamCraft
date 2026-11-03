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
    UnifiedPushRecovery.onRegisterRequested(context)
    if (UnifiedPush.getAckDistributor(context) == null) {
      UnifiedPush.getDistributors(context).firstOrNull()?.let { UnifiedPush.saveDistributor(context, it) }
    }
    UnifiedPush.register(context)
  }

  override fun unregister(context: Context) {
    UnifiedPushRecovery.onSelfUnregister(context, isStillWanted = false)
    if (UnifiedPush.getAckDistributor(context) != null) UnifiedPush.unregister(context)
  }

  /** Package names of the installed distributors. */
  fun distributors(context: Context): List<String> = UnifiedPush.getDistributors(context)

  /** The distributor in use (acknowledged, or chosen and waiting for its answer). */
  fun currentDistributor(context: Context): String? =
    UnifiedPush.getAckDistributor(context) ?: UnifiedPush.getSavedDistributor(context)

  /** Switches to another installed distributor; its new endpoint arrives as with [register]. */
  fun selectDistributor(context: Context, packageName: String) {
    if (packageName !in distributors(context) || packageName == UnifiedPush.getAckDistributor(context)) return
    // The old distributor stops delivering to us; the new one registers from scratch.
    if (UnifiedPush.getAckDistributor(context) != null) {
      UnifiedPushRecovery.onSelfUnregister(context, isStillWanted = true)
      UnifiedPush.unregister(context)
    }
    UnifiedPush.saveDistributor(context, packageName)
    UnifiedPush.register(context)
  }
}
