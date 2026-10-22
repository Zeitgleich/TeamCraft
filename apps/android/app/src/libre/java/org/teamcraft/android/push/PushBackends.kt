package org.teamcraft.android.push

/** Builds without Google services: UnifiedPush only. */
object PushBackends {
  fun all(): List<PushBackend> = listOf(UnifiedPushBackend)
}
