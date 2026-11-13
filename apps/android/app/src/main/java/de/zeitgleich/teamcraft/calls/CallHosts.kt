package de.zeitgleich.teamcraft.calls

import android.util.Log
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.serialization.Serializable
import kotlinx.serialization.SerializationException
import kotlinx.serialization.json.JsonElement
import de.zeitgleich.teamcraft.core.ChatClient
import de.zeitgleich.teamcraft.core.CoreRepository
import de.zeitgleich.teamcraft.core.User
import de.zeitgleich.teamcraft.core.WireJson
import uniffi.teamcraft_ffi.MeetingHostAction
import uniffi.teamcraft_ffi.MeetingHosts
import uniffi.teamcraft_ffi.TeamCraftException

/**
 * Co-hosts and handing a meeting over in its call (#1720; docs/meetings.md, "Hosts"): who hosts
 * the meeting whose chat the call is in, the organizer's participant menu, and the question whom to
 * hand it to when the organizer leaves with people waiting in the lobby. The core signs the host
 * list with the user's identity key and publishes it; this class only reads and asks.
 */
class CallHosts(
  private val core: CoreRepository,
  private val scope: CoroutineScope,
) {
  private val chat = ChatClient { core.core() }
  private val answered = MutableStateFlow(0L)

  /** Moves on every change of the store: the hosts and the last change's outcome are read again. */
  val changes: StateFlow<Long> get() = core.changeCount

  /** Moves each time a change of the hosts was answered here, so its outcome is read again. */
  val answers: StateFlow<Long> = answered

  /** Who hosts the meeting whose chat is `conversationId`; `null` outside a meeting's call. */
  suspend fun hosts(conversationId: String): MeetingHosts? =
    quietly("reading the meeting's hosts") { core.core().meetingHosts(conversationId) }

  /** How the last change of the hosts went on this device ([HostChangeStatus]); `null` before one. */
  suspend fun changeStatus(conversationId: String): HostChangeStatus? =
    quietly("reading the host change") { core.core().meetingHostsChange(conversationId) }
      ?.let(HostChangeStatus::of)

  /** "Make co-host", "Remove as co-host" or "Hand over host role" for `userId` (organizer only). */
  fun change(conversationId: String, userId: String, action: MeetingHostAction) {
    scope.launch {
      quietly("changing the meeting's hosts") { core.core().changeMeetingHost(conversationId, userId, action) }
      answered.value += 1
    }
  }

  /** The people in the call who could host: known members, no guests, no bots, not the user. */
  suspend fun candidates(participantIds: List<String>): List<User> {
    val snapshot = quietly("reading the people") { withContext(Dispatchers.Default) { chat.snapshot() } } ?: return emptyList()
    val known = snapshot.users.associateBy { it.id }
    return participantIds
      .filter { it != snapshot.me.id }
      .mapNotNull { known[it] }
      .filter { !it.isGuest && !it.isBot }
  }

  /**
   * What to ask when the organizer leaves the call of `conversationId`: people wait in the lobby,
   * nobody else hosts and somebody in the call could take over (the core's rule, as on desktop and
   * web). `null`: just leave. A failed read leaves too: the user asked to go.
   */
  suspend fun leaveOffer(hosts: MeetingHosts, conversationId: String, participantIds: List<String>): LeaveOffer? {
    val candidates = candidates(participantIds)
    if (candidates.isEmpty()) return null
    val waiting = waitingInLobby(hosts.meetingId) ?: return null
    val isOffered = quietly("checking the handover") {
      core.core().offersMeetingHandover(conversationId, waiting.toUInt(), candidates.map { it.id })
    } ?: false
    return if (isOffered) LeaveOffer(waiting, candidates.take(MAX_HANDOVER_CHOICES)) else null
  }

  /** How many wait in the meeting's lobby (`GET /meetings/{id}/lobby`, hosts only). */
  private suspend fun waitingInLobby(meetingId: String): Int? =
    quietly("reading the lobby") {
      WireJson.decodeFromString(LobbyList.serializer(), chat.api("GET", "/meetings/$meetingId/lobby")).entries.size
    }

  private suspend fun <T> quietly(what: String, read: suspend () -> T): T? =
    try {
      read()
    } catch (error: TeamCraftException) {
      Log.w(TAG, "$what failed", error)
      null
    } catch (error: SerializationException) {
      Log.w(TAG, "$what failed", error)
      null
    } catch (error: IllegalStateException) {
      if (error is CancellationException) throw error
      Log.w(TAG, "$what failed", error)
      null
    }

  @Serializable
  private class LobbyList(val entries: List<JsonElement> = emptyList())

  private companion object {
    const val TAG = "TeamCraftHosts"

    /** Up to this many people are offered to hand the meeting to, as on desktop and web. */
    const val MAX_HANDOVER_CHOICES = 5
  }
}

/** Leaving would leave `waiting` people in the lobby without a host: whom to hand the meeting to. */
data class LeaveOffer(val waiting: Int, val candidates: List<User>)

/** How the last change of the hosts went, as the core says it. */
sealed interface HostChangeStatus {
  data object Saving : HostChangeStatus

  data object Saved : HostChangeStatus

  /** The core's words for the failure (shown under a translated lead-in). */
  data class Failed(val reason: String) : HostChangeStatus

  companion object {
    fun of(text: String): HostChangeStatus = when (text) {
      "saving" -> Saving
      "saved" -> Saved
      else -> Failed(text)
    }
  }
}
