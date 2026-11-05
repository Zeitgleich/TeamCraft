package de.zeitgleich.teamcraft.core

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

/** The workspace's rule for "send later" (docs/scheduled-messages.md); the server enforces it. */
@Serializable
data class SchedulePolicy(
  @SerialName("is_enabled") val isEnabled: Boolean = false,
  @SerialName("max_horizon_days") val maxHorizonDays: Int = 365,
  @SerialName("max_per_user") val maxPerUser: Int = 50,
)

/** A message waiting for its time. The core decides [status]; the app only words it. */
@Serializable
data class ScheduledMessage(
  val id: String,
  val conversation: String,
  @SerialName("conversation_name") val conversationName: String = "",
  val body: String = "",
  @SerialName("due_at") val dueAt: Long,
  val status: String = "unknown",
) {
  /** Whether the message waits for the user to do something. */
  val needsAttention: Boolean
    get() = status in setOf(STATUS_POLICY_OFF, STATUS_NOT_A_MEMBER, STATUS_NO_PERMISSION, STATUS_CONVERSATION_GONE, STATUS_THREAD_GONE)

  /** Whether sending it can still make it go out. */
  val canGoOut: Boolean get() = status in setOf(STATUS_WAITING, STATUS_NEEDS_SEALING, STATUS_STALE, STATUS_POLICY_OFF)

  val canEdit: Boolean get() = canGoOut && status != STATUS_POLICY_OFF

  companion object {
    const val STATUS_WAITING = "waiting"
    const val STATUS_NEEDS_SEALING = "needs_sealing"
    const val STATUS_STALE = "stale"
    const val STATUS_POLICY_OFF = "policy_off"
    const val STATUS_NOT_A_MEMBER = "not_a_member"
    const val STATUS_NO_PERMISSION = "no_permission"
    const val STATUS_CONVERSATION_GONE = "conversation_gone"
    const val STATUS_THREAD_GONE = "thread_gone"
    const val STATUS_UNREADABLE = "unreadable"
  }
}

/** The list, the rule and the server's clock as this device knows it. */
@Serializable
data class ScheduledOverview(
  val messages: List<ScheduledMessage> = emptyList(),
  val policy: SchedulePolicy = SchedulePolicy(),
  @SerialName("server_now") val serverNow: Long = 0,
)
