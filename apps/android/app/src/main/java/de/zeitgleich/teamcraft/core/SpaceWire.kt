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
