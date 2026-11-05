package de.zeitgleich.teamcraft.core

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

/** The workspace's rule for "send later" (docs/scheduled-messages.md). */
@Serializable
data class SchedulePolicy(
  @SerialName("is_enabled") val isEnabled: Boolean = false,
  @SerialName("max_horizon_days") val maxHorizonDays: Int = 365,
  @SerialName("late_limit_hours") val lateLimitHours: Int = 24,
)

/** A message waiting for its time. The core decides [status]; the app only words it. */
@Serializable
data class ScheduledMessage(
  val id: String,
  val conversation: String,
  @SerialName("conversation_name") val conversationName: String = "",
  val body: String,
  @SerialName("due_at") val dueAt: Long,
  val status: String = "unknown",
) {
  /** The status as shown: one that was waiting when it was read is due once its time passed. */
  fun shownStatus(nowMs: Long): String = if (status == STATUS_WAITING && dueAt <= nowMs) STATUS_DUE else status

  /** Whether the message waits for the user to do something. */
  val needsAttention: Boolean get() = status in setOf(STATUS_MISSED, STATUS_POLICY_OFF, STATUS_UNAVAILABLE)

  val isAvailable: Boolean get() = status != STATUS_UNAVAILABLE

  companion object {
    const val STATUS_WAITING = "waiting"
    const val STATUS_DUE = "due"
    const val STATUS_MISSED = "missed"
    const val STATUS_POLICY_OFF = "policy_off"
    const val STATUS_UNAVAILABLE = "unavailable"
  }
}

/** The list, the rule and the server's clock as this device knows it. */
@Serializable
data class ScheduledOverview(
  val messages: List<ScheduledMessage> = emptyList(),
  val policy: SchedulePolicy = SchedulePolicy(),
  @SerialName("server_now") val serverNow: Long = 0,
) {
  /** The next time a device of this user has something to send, for the background work. */
  val maybeNextDueAt: Long?
    get() = messages
      .filter { it.status == ScheduledMessage.STATUS_WAITING || it.status == ScheduledMessage.STATUS_DUE }
      .minOfOrNull { it.dueAt }
}
