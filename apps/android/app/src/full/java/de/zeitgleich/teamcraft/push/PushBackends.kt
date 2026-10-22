package de.zeitgleich.teamcraft.push

/** Google Play builds: FCM when configured, UnifiedPush otherwise. */
object PushBackends {
  fun all(): List<PushBackend> = listOf(FcmBackend, UnifiedPushBackend)
}
