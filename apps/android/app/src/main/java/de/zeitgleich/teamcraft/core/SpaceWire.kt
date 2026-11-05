package de.zeitgleich.teamcraft.core

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

/** One call of the user's history (teamcraft-client-api `calls`). */
@Serializable
data class CallHistoryItem(
  val id: String,
  val conversation: String,
  /** `missed`, `incoming` or `outgoing`. */
  val direction: String,
  @SerialName("started_at") val startedAt: Long,
)

/** Somebody in a space, as far as the user may see it. */
@Serializable
data class SpaceMember(
  val user: String,
  @SerialName("display_name") val displayName: String,
  @SerialName("is_guest") val isGuest: Boolean = false,
  @SerialName("is_bot") val isBot: Boolean = false,
)

/** The member list of a space: `loading`, `hidden` (guests may not see it) or `loaded`. */
@Serializable
data class SpaceRoster(
  val state: String,
  val members: List<SpaceMember> = emptyList(),
)
