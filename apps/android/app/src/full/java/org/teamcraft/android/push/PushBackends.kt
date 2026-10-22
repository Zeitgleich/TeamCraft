package org.teamcraft.android.push

/** Google Play builds: FCM when configured, UnifiedPush otherwise. */
object PushBackends {
  fun all(): List<PushBackend> = listOf(FcmBackend, UnifiedPushBackend)
}
