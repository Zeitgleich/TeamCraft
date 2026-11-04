package de.zeitgleich.teamcraft.calls

/**
 * A call ringing on this device: from a `call` wake-up (with the call's ID) or from the gateway
 * while the app runs (without one).
 */
data class IncomingRing(
  val conversationId: String,
  val maybeCallId: String?,
  /** The conversation's name, or the caller's for a DM. */
  val displayName: String,
  val maybeCallerName: String?,
  /**
   * The device knows the conversation as one it is in. Otherwise the ring is only the server's word:
   * it alerts, but offers no answer (no full-screen screen, no Telecom call), as on iOS.
   */
  val isVerified: Boolean,
) {
  /**
   * Whether a `dismiss` wake-up for `callId` ends this ring. Only the same call: a late dismissal
   * of an earlier call must not silence a new one (a gateway ring ends through the gateway).
   */
  fun isEndedBy(callId: String): Boolean = maybeCallId == callId

  /** What a second ring report does to this one: the same conversation adds the call's ID. */
  fun merged(other: IncomingRing): IncomingRing? =
    if (other.conversationId == conversationId) copy(maybeCallId = maybeCallId ?: other.maybeCallId, isVerified = isVerified || other.isVerified) else null

  /**
   * Whether the incoming-call screen opens right away. Android shows a ring's full-screen intent only
   * while the phone is locked or its screen off; in use, it is a heads-up instead, and the app open
   * in front would not show the ring at all (#1272). Unverified rings have no screen.
   */
  fun opensScreenAtOnce(isAppVisible: Boolean): Boolean = isVerified && isAppVisible
}
