package de.zeitgleich.teamcraft.push

/** Builds without Google services: UnifiedPush only. */
object PushBackends {
  fun all(): List<PushBackend> = listOf(UnifiedPushBackend)
}
