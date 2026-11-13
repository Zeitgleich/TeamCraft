package de.zeitgleich.teamcraft.calls

import android.util.Log
import androidx.annotation.StringRes
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.ChatClient
import de.zeitgleich.teamcraft.core.ChatSnapshot
import de.zeitgleich.teamcraft.core.CoreRepository
import de.zeitgleich.teamcraft.core.User
import de.zeitgleich.teamcraft.core.WireJson
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
import kotlinx.serialization.SerializationException
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import uniffi.teamcraft_ffi.MeetingHostAction
import uniffi.teamcraft_ffi.MeetingHosts
import uniffi.teamcraft_ffi.MeetingPart
import uniffi.teamcraft_ffi.TeamCraftException

/**
 * Co-hosts and handing a meeting over in its call (#1720), as desktop and web do it
 * (docs/meetings.md, "Hosts"): who hosts the meeting of the call, for the tiles' "Host" and
 * "Co-host", the organizer's "Make co-host", "Remove as co-host" and "Hand over host role" (the
 * core signs the new host list, e2ee.md §4.6), and the question before the organizer leaves with
 * people waiting in the lobby and nobody else to let them in.
 */
class CallHosts(
  private val core: CoreRepository,
  private val scope: CoroutineScope,
) {
  private val chat = ChatClient { core.core() }
  private val current = MutableStateFlow<MeetingHosts?>(null)
  private val changing = MutableStateFlow<HostsChange>(HostsChange.Idle)

  /** Counts the calls: a reading that comes back after the call ended is dropped. */
  @Volatile private var generation = 0

  /** Who hosts the meeting of the call; `null` for a call outside a meeting (or before it is read). */
  val hosts: StateFlow<MeetingHosts?> = current

  /** How the last change of the hosts on this device went. */
  val change: StateFlow<HostsChange> = changing

  /** Reads who hosts the meeting of the call in `conversationId` again (it stays as it was offline). */
  suspend fun refresh(conversationId: String) {
    val call = generation
    try {
      val read = withContext(Dispatchers.Default) { core.core().meetingHosts(conversationId) }
      if (call == generation) current.value = read
    } catch (error: TeamCraftException) {
      Log.w(TAG, "the meeting's hosts could not be read", error)
    } catch (error: IllegalStateException) {
      if (error is CancellationException) throw error
    }
  }

  /** The call ended: nothing of it is shown in the next one. */
  fun reset() {
    generation++
    current.value = null
    changing.value = HostsChange.Idle
  }

  /**
   * The organizer's menu on `userId`: the core signs and publishes the new hosts; this follows how
   * it went and reads the hosts again once it is through.
   */
  fun change(conversationId: String, userId: String, action: MeetingHostAction) {
    if (changing.value == HostsChange.Saving) return
    changing.value = HostsChange.Saving
    scope.launch {
      changing.value = try {
        core.core().changeMeetingHost(conversationId, userId, action)
        awaitHostsChange(core, conversationId)
      } catch (error: TeamCraftException) {
        Log.w(TAG, "the meeting's hosts could not be changed", error)
        HostsChange.Failed(null, failureText(error))
      } catch (error: IllegalStateException) {
        if (error is CancellationException) throw error
        HostsChange.Failed(null)
      }
      refresh(conversationId)
    }
  }

  /**
   * "Hand over to … and leave" (the leave question): the core takes the signed handover on its
   * own queue, which goes on after the call; then the user leaves. A failure is only logged, as
   * desktop and web leave at once too.
   */
  suspend fun handOverBeforeLeaving(conversationId: String, userId: String) {
    try {
      withTimeoutOrNull(OFFER_TIMEOUT_MILLIS) { core.core().changeMeetingHost(conversationId, userId, MeetingHostAction.HAND_OVER) }
    } catch (error: TeamCraftException) {
      Log.w(TAG, "the meeting could not be handed over", error)
    } catch (error: IllegalStateException) {
      if (error is CancellationException) throw error
    }
  }

  /** The others in the call, for the host menu; empty while the store can't be read. */
  suspend fun people(participantIds: List<String>): List<User> =
    try {
      val snapshot = withContext(Dispatchers.Default) { chat.snapshot() }
      participantIds.distinct().filter { it != snapshot.me.id }.mapNotNull { snapshot.user(it) }
    } catch (error: TeamCraftException) {
      emptyList()
    } catch (error: SerializationException) {
      emptyList()
    } catch (error: IllegalStateException) {
      if (error is CancellationException) throw error
      emptyList()
    }

  /** The failure shown under the menu goes away. */
  fun dismissFailure() {
    if (changing.value is HostsChange.Failed) changing.value = HostsChange.Idle
  }

  /**
   * What to ask before the user leaves the call of `conversationId` with `participantIds` in it:
   * a handover when the user organizes its meeting, people wait in the lobby, nobody else hosts
   * it and somebody here could (the core decides, as desktop and web). `null` to just leave; also
   * when the answer takes too long, so leaving never hangs on the network.
   */
  suspend fun handoverOffer(conversationId: String, participantIds: List<String>): HandoverOffer? {
    val hosts = current.value?.takeIf { it.ownPart == MeetingPart.ORGANIZER } ?: return null
    return withTimeoutOrNull(OFFER_TIMEOUT_MILLIS) {
      try {
        val snapshot = withContext(Dispatchers.Default) { chat.snapshot() }
        val candidates = handoverCandidates(participantIds, snapshot)
        if (candidates.isEmpty()) return@withTimeoutOrNull null
        val waiting = lobbyWaitingCount(chat.api("GET", "/meetings/${hosts.meetingId}/lobby")) ?: 0
        if (waiting == 0) return@withTimeoutOrNull null
        val offers = core.core().offersMeetingHandover(conversationId, waiting.toUInt(), candidates.map { it.id })
        if (offers) HandoverOffer(waiting, candidates.take(MAX_HANDOVER_CHOICES)) else null
      } catch (error: TeamCraftException) {
        Log.w(TAG, "the handover could not be checked", error)
        null
      } catch (error: SerializationException) {
        null
      } catch (error: IllegalStateException) {
        if (error is CancellationException) throw error
        null
      }
    }
  }

  private companion object {
    const val TAG = "TeamCraftCalls"
    const val OFFER_TIMEOUT_MILLIS = 4_000L

    @StringRes
    fun failureText(error: TeamCraftException): Int = when (error) {
      is TeamCraftException.Network -> R.string.error_network
      else -> R.string.error_generic
    }
  }
}

private const val CHANGE_SAVING = "saving"
private const val CHANGE_SAVED = "saved"
private const val CHANGE_POLLS = 40
private const val CHANGE_POLL_MILLIS = 250L

/**
 * Waits for the core's signed change of the hosts of `conversationId` to be saved or refused (it
 * runs on the core's own queue): the host menu and the handover after leaving (#1859).
 */
internal suspend fun awaitHostsChange(core: CoreRepository, conversationId: String): HostsChange {
  repeat(CHANGE_POLLS) {
    when (val state = core.core().meetingHostsChange(conversationId)) {
      null, CHANGE_SAVING -> delay(CHANGE_POLL_MILLIS)
      CHANGE_SAVED -> return HostsChange.Idle
      else -> return HostsChange.Failed(state)
    }
  }
  return HostsChange.Idle
}

/** How many people of the call the leave question offers to hand the meeting to, as on desktop. */
internal const val MAX_HANDOVER_CHOICES = 5

/** How the last change of the hosts went: saving, or why it failed. */
sealed interface HostsChange {
  data object Idle : HostsChange

  data object Saving : HostsChange

  /** [maybeReason]: the core's words; without them [fallback] says it. */
  data class Failed(val maybeReason: String?, @StringRes val fallback: Int = R.string.error_generic) : HostsChange
}

/** The organizer leaves with `waiting` people in the lobby: whom the meeting can go to. */
data class HandoverOffer(val waiting: Int, val candidates: List<User>)

/** A person's part in the meeting, as their tile says it; `null` for an ordinary participant. */
internal fun hostLabelOf(hosts: MeetingHosts?, userId: String): Int? = when {
  hosts == null -> null
  hosts.organizerUserId == userId -> R.string.call_tile_host
  userId in hosts.coHostUserIds -> R.string.call_tile_co_host
  else -> null
}

/**
 * What the organizer's menu offers on somebody: "Make co-host" or "Remove as co-host", and "Hand
 * over host role". Nothing for anybody else's menu, nor on guests and bots (the server refuses them
 * as hosts) or the organizer.
 */
internal fun hostActionsFor(hosts: MeetingHosts?, user: User): List<MeetingHostAction> {
  if (hosts == null || hosts.ownPart != MeetingPart.ORGANIZER) return emptyList()
  if (user.isGuest || user.isBot || user.id == hosts.organizerUserId) return emptyList()
  val coHost = if (user.id in hosts.coHostUserIds) MeetingHostAction.REMOVE_CO_HOST else MeetingHostAction.MAKE_CO_HOST
  return listOf(coHost, MeetingHostAction.HAND_OVER)
}

/** The others in the call who could take the meeting over: members of this server, not guests or bots. */
internal fun handoverCandidates(participantIds: List<String>, snapshot: ChatSnapshot): List<User> =
  participantIds
    .distinct()
    .filter { it != snapshot.me.id }
    .mapNotNull { snapshot.user(it) }
    .filter { !it.isGuest && !it.isBot && it.isOffered }

/** How many wait in a meeting's lobby, from the server's `GET /meetings/{id}/lobby`; `null` if unreadable. */
internal fun lobbyWaitingCount(json: String): Int? =
  try {
    WireJson.parseToJsonElement(json).jsonObject["entries"]?.jsonArray?.size
  } catch (error: SerializationException) {
    null
  } catch (error: IllegalArgumentException) {
    null
  }
