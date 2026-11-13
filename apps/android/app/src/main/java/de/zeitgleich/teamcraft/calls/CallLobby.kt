package de.zeitgleich.teamcraft.calls

import android.util.Log
import androidx.annotation.StringRes
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.CoreRepository
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import uniffi.teamcraft_ffi.LobbyDecision
import uniffi.teamcraft_ffi.LobbyDecisionOutcome
import uniffi.teamcraft_ffi.LobbyPerson
import uniffi.teamcraft_ffi.LobbyPersonStatus
import uniffi.teamcraft_ffi.MeetingLobby
import uniffi.teamcraft_ffi.TeamCraftException

/**
 * The lobby of the meeting in the call (#1859), as desktop and web have it: who waits, for the
 * organizer and the co-hosts only (the core reads it for hosts and answers `null` to anybody
 * else), and their "Admit" and "Deny". The core does the admitting: it tells the server and lets
 * the person's devices into the meeting chat's encryption on this device's own decision (e2ee.md
 * §4.6, "Meeting guests"). Read when the call's people change, on a knock and after a decision;
 * nothing polls, except a few reads for a guest's key code the core is still making.
 */
class CallLobby(
  private val core: CoreRepository,
  private val scope: CoroutineScope,
) {
  private val current = MutableStateFlow<MeetingLobby?>(null)
  private val deciding = MutableStateFlow<LobbyDeciding>(LobbyDeciding.Idle)
  private var maybeCodeRetry: Job? = null

  /** Counts the calls: a reading that comes back after the call ended is dropped. */
  @Volatile private var generation = 0

  /** The lobby of the call's meeting if the user hosts it; `null` otherwise (or before it is read). */
  val lobby: StateFlow<MeetingLobby?> = current

  /** The decision on its way, or why the last one failed. */
  val decision: StateFlow<LobbyDeciding> = deciding

  /** Reads the lobby of the meeting whose chat is `conversationId` again; offline it stays as it was. */
  suspend fun refresh(conversationId: String) {
    val call = generation
    try {
      val read = withContext(Dispatchers.Default) { core.core().meetingLobby(conversationId) }
      if (call != generation) return
      current.value = read
      if (read != null && isWaitingForKeyCode(read)) retryForKeyCodes(conversationId, call)
    } catch (error: TeamCraftException) {
      Log.w(TAG, "the meeting's lobby could not be read", error)
    } catch (error: IllegalStateException) {
      if (error is CancellationException) throw error
    }
  }

  /** Somebody knocked at `meetingId` (the call listener): read again if it is the call's meeting. */
  fun onKnock(conversationId: String, meetingId: String) {
    if (current.value?.meetingId != meetingId) return
    scope.launch { refresh(conversationId) }
  }

  /** "Admit" or "Deny" on `person`; one at a time. */
  fun decide(conversationId: String, person: LobbyPerson, decision: LobbyDecision) {
    if (deciding.value is LobbyDeciding.Deciding) return
    deciding.value = LobbyDeciding.Deciding(person.userId)
    scope.launch {
      deciding.value = try {
        val outcome = withContext(Dispatchers.Default) { core.core().decideLobbyEntry(conversationId, person.userId, decision) }
        if (outcome == LobbyDecisionOutcome.NO_LONGER_WAITING) {
          LobbyDeciding.Failed(person.displayName, R.string.call_lobby_no_longer_waiting)
        } else {
          LobbyDeciding.Idle
        }
      } catch (error: TeamCraftException) {
        Log.w(TAG, "a lobby decision failed", error)
        LobbyDeciding.Failed(person.displayName, failureText(error))
      } catch (error: IllegalStateException) {
        if (error is CancellationException) throw error
        LobbyDeciding.Failed(person.displayName, R.string.error_generic)
      }
      refresh(conversationId)
    }
  }

  /** The failure under the lobby goes away. */
  fun dismissFailure() {
    if (deciding.value is LobbyDeciding.Failed) deciding.value = LobbyDeciding.Idle
  }

  /** The call ended: nothing of it is shown in the next one. */
  fun reset() {
    generation++
    maybeCodeRetry?.cancel()
    maybeCodeRetry = null
    current.value = null
    deciding.value = LobbyDeciding.Idle
  }

  /** The core pins a guest's key on its own queue: a few reads later its code is there. */
  private fun retryForKeyCodes(conversationId: String, call: Int) {
    if (maybeCodeRetry?.isActive == true) return
    maybeCodeRetry = scope.launch {
      repeat(KEY_CODE_POLLS) {
        delay(KEY_CODE_POLL_MILLIS)
        if (call != generation) return@launch
        val read = try {
          withContext(Dispatchers.Default) { core.core().meetingLobby(conversationId) }
        } catch (error: TeamCraftException) {
          return@launch
        }
        if (call != generation) return@launch
        current.value = read
        if (read == null || !isWaitingForKeyCode(read)) return@launch
      }
    }
  }

  private companion object {
    const val TAG = "TeamCraftCalls"
    const val KEY_CODE_POLLS = 8
    const val KEY_CODE_POLL_MILLIS = 500L

    @StringRes
    fun failureText(error: TeamCraftException): Int = when (error) {
      is TeamCraftException.Network -> R.string.error_network
      else -> R.string.call_lobby_admit_failed
    }
  }
}

/** A decision in the lobby: on its way for somebody, or failed (whose, and why). */
sealed interface LobbyDeciding {
  data object Idle : LobbyDeciding

  data class Deciding(val userId: String) : LobbyDeciding

  data class Failed(val name: String, @StringRes val reason: Int) : LobbyDeciding
}

/** Whether a guest of `lobby` is ready to be admitted but its key code isn't made yet. */
internal fun isWaitingForKeyCode(lobby: MeetingLobby): Boolean =
  lobby.people.any { person ->
    val status = person.status
    status is LobbyPersonStatus.Guest && status.maybeKeyCode == null
  }
