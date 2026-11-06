package de.zeitgleich.teamcraft.calls

/**
 * How long a one-to-one call waits after the other person left before it ends: long enough for a
 * dropped connection that comes back, short enough that nobody sits in an empty call.
 */
const val ALONE_END_MS = 4_000L

/**
 * Whether this device should leave a call it is alone in now: in a direct message, like a phone
 * call, once the other person was there and left (not while they are still on their way in, and
 * not while their connection is coming back). Group calls and meetings stay open for whoever comes.
 */
fun isLeftAlone(isOneToOne: Boolean, hadOthers: Boolean, participantCount: Int, reconnectingCount: Int): Boolean =
  isOneToOne && hadOthers && participantCount == 0 && reconnectingCount == 0
