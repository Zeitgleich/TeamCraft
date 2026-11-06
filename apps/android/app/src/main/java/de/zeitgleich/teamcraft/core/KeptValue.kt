package de.zeitgleich.teamcraft.core

/**
 * A value read once per change of one account's store and shared until the next one: kept with the
 * account's generation (a new one every time a core opens or closes) and the change count. A value
 * of another account is never handed out, also not as the "kept from before" one.
 */
class KeptValue<T : Any> {
  @Volatile private var maybeKept: Kept<T>? = null

  /** The value kept for exactly this generation and change, if any. */
  fun at(generation: Long, changes: Long): T? =
    maybeKept?.takeIf { it.generation == generation && it.changes == changes }?.value

  /** The value read last for this generation, maybe from before its newest change. */
  fun latest(generation: Long): T? = maybeKept?.takeIf { it.generation == generation }?.value

  fun keep(generation: Long, changes: Long, value: T) {
    maybeKept = Kept(generation, changes, value)
  }

  fun clear() {
    maybeKept = null
  }

  private data class Kept<T>(val generation: Long, val changes: Long, val value: T)
}
