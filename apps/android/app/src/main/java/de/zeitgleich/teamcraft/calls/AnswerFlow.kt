package de.zeitgleich.teamcraft.calls

import androidx.annotation.StringRes
import de.zeitgleich.teamcraft.R
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.launch

/** Who answered a ring. */
enum class AnswerSource {
  /** "Answer" on the incoming-call notification or on the incoming-call screen. */
  APP,

  /**
   * Telecom, for the system's call UI, a watch, a headset or the car. Telecom answered its call
   * already; it waits for its callback at most 5 s (Core-Telecom `addCall`).
   */
  TELECOM,
}

/**
 * What answering a ring does, from where this device's call stands (#647, #1272). Kept apart from
 * Telecom and the engine so the paths are tested; [CallController] carries them out.
 */
enum class AnswerRoute {
  /** In a call or joining one already: a second answer does nothing. */
  IGNORE,

  /** The ring's Telecom call is there: answer it, then join. */
  ANSWER_TELECOM_THEN_JOIN,

  /**
   * Join now: Telecom answered already, or it is still adding the ring (its call is answered once
   * it is added, so it doesn't keep ringing).
   */
  JOIN,

  /** No Telecom call (it failed, or the process restarted since the ring): join in an outgoing one. */
  ADD_TELECOM_CALL,
  ;

  companion object {
    fun of(source: AnswerSource, isInCall: Boolean, isJoining: Boolean, hasTelecomCall: Boolean, isTelecomAdding: Boolean): AnswerRoute =
      when {
        isInCall || isJoining -> IGNORE
        source == AnswerSource.TELECOM -> JOIN
        hasTelecomCall -> ANSWER_TELECOM_THEN_JOIN
        isTelecomAdding -> JOIN
        else -> ADD_TELECOM_CALL
      }
  }
}

/** The steps of an answer, as [CallController] does them with Telecom and the engine. */
interface AnswerSteps {
  /** Answers the ring's Telecom call. */
  suspend fun answerTelecomCall()

  /** Joins in the engine and shows the call; `false` if joining failed (the failure is reported). */
  suspend fun join(): Boolean

  /** Adds an outgoing Telecom call that joins (and cleans up after itself if it doesn't). */
  suspend fun addTelecomCall()

  /** A failed join: ends the Telecom call, the ongoing-call service and its notification. */
  suspend fun endCall()
}

/**
 * Carries out [route] in [scope] and returns at once: Telecom gives its answer callback 5 s and
 * tears the call down after that, and joining (microphone, codec, connecting) takes longer. `null`
 * for [AnswerRoute.IGNORE].
 */
fun startAnswer(scope: CoroutineScope, route: AnswerRoute, steps: AnswerSteps): Job? = when (route) {
  AnswerRoute.IGNORE -> null
  AnswerRoute.ADD_TELECOM_CALL -> scope.launch { steps.addTelecomCall() }
  AnswerRoute.ANSWER_TELECOM_THEN_JOIN, AnswerRoute.JOIN -> scope.launch {
    if (route == AnswerRoute.ANSWER_TELECOM_THEN_JOIN) steps.answerTelecomCall()
    if (!steps.join()) steps.endCall()
  }
}

/** Why joining a call failed, as the call screen (or a notification) tells the user. */
enum class JoinFailure(@StringRes val text: Int) {
  /** The microphone permission was refused (or not answered in time). */
  MICROPHONE(R.string.call_join_failed_microphone),
  NETWORK(R.string.call_join_failed_network),
  DEVICE_LIMIT(R.string.error_call_device_limit),
  ENCRYPTION_PENDING(R.string.error_call_encryption_pending),
  OTHER(R.string.call_join_failed),
}
