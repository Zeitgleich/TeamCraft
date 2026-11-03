package de.zeitgleich.teamcraft.calls

import android.util.Log
import androidx.annotation.StringRes
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import kotlinx.serialization.SerializationException
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.ChatClient
import de.zeitgleich.teamcraft.core.ChatSnapshot
import de.zeitgleich.teamcraft.core.CoreRepository
import de.zeitgleich.teamcraft.core.User
import de.zeitgleich.teamcraft.ui.MainViewModel
import uniffi.teamcraft_ffi.TeamCraftException

/**
 * "Add people" in a DM or group DM call and following a move of the call into a meeting room
 * (docs/meetings.md, "A call becomes a meeting"). The core starts the room, founds its group and
 * checks every move; this class keeps the dialog's and the banner's state. Nothing moves on the
 * server's `call_move` alone: only an announcement from the DM's group (`callMoveOffer`) that
 * `acceptCallMove` confirmed, and for others only on the user's tap.
 */
class CallMoves(
  private val core: CoreRepository,
  private val scope: CoroutineScope,
  /** The conversation of the call this device is in; `null` when idle. */
  private val activeConversation: () -> String?,
  /** The other people in that call, as the call engine verified them. */
  private val callPeople: () -> List<String>,
  /** Leaves the DM's call and joins the room's; `false` if joining failed. */
  private val follow: suspend (from: String, to: String) -> Boolean,
) {
  private val chat = ChatClient { core.core() }
  private val pending = MutableStateFlow<PendingMove?>(null)
  private val offer = MutableStateFlow<CallMoveOffer?>(null)
  private val adding = MutableStateFlow<AddPeopleState>(AddPeopleState.Idle)
  private val failure = MutableStateFlow<Int?>(null)
  private val accepting = MutableStateFlow(false)
  private val checkLock = Mutex()

  /** A move another member announced, until joined, refused or the call ended. */
  val moveOffer: StateFlow<CallMoveOffer?> = offer

  /** Where the "Add people" request stands. */
  val addState: StateFlow<AddPeopleState> = adding

  /** Why following a move failed (string resource), until dismissed. */
  val moveFailure: StateFlow<Int?> = failure

  /** The move is being checked and the room's call joined. */
  val isAccepting: StateFlow<Boolean> = accepting

  /** Asks for the announcement again on every change while a move waits for it. */
  fun start() {
    scope.launch {
      core.changeCount.collect {
        if (pending.value != null && offer.value == null) check()
      }
    }
  }

  /**
   * Who can be added to the call of `conversationId`: `null` unless it is a DM or group DM (a
   * channel or meeting call takes more people through its own members).
   */
  suspend fun choice(conversationId: String): AddPeopleChoice? {
    val snapshot = readSnapshot() ?: return null
    val conversation = snapshot.conversation(conversationId)?.takeIf { it.isDirect } ?: return null
    // The DM's own members are offered nowhere: those who aren't in the call join it from the DM.
    val members = conversation.memberIds + snapshot.me.id
    val candidates = snapshot.users
      .filter { it.id !in members && !it.isBot }
      .sortedBy { it.displayName.lowercase() }
    return AddPeopleChoice(conversationId, snapshot.me.id, candidates)
  }

  /**
   * Starts the meeting room with the people in the call and `added`, and moves the call there.
   * `requestKey` is the dialog's: a retry after a lost answer gets the same room.
   */
  fun add(choice: AddPeopleChoice, added: List<String>, title: String, requestKey: String) {
    if (adding.value == AddPeopleState.Pending || added.isEmpty()) return
    adding.value = AddPeopleState.Pending
    scope.launch {
      // Who is in the call now (plus the user); the core keeps only members of the DM's group.
      val inCall = (callPeople() + choice.myUserId).distinct()
      adding.value = try {
        val to = core.core().addPeopleToCall(choice.conversationId, inCall, added, title, requestKey)
        // This device follows its own move too, after the same check as everybody else's.
        moved(choice.conversationId, to)
        AddPeopleState.Done
      } catch (error: TeamCraftException) {
        Log.w(TAG, "adding people to the call failed", error)
        AddPeopleState.Failed(MainViewModel.errorText(error))
      } catch (error: IllegalStateException) {
        if (error is CancellationException) throw error
        // No account any more (signed out meanwhile).
        Log.w(TAG, "adding people to the call failed", error)
        AddPeopleState.Failed(R.string.error_generic)
      }
    }
  }

  /** The dialog closed or showed its result: the next one starts fresh. */
  fun addShown() {
    if (adding.value != AddPeopleState.Pending) adding.value = AddPeopleState.Idle
  }

  /** The server says the call moved (`onCallMoved`), or this device moved it. */
  fun moved(from: String, to: String) {
    if (activeConversation() != from) return
    val move = PendingMove(from, to)
    if (pending.value != move) {
      pending.value = move
      offer.value = null
      failure.value = null
    }
    scope.launch { check() }
  }

  /** "Join the meeting room" on the banner. */
  fun accept() {
    val move = pending.value ?: return
    scope.launch { accept(move) }
  }

  fun dismissFailure() {
    failure.value = null
  }

  /** The call ended or moved: nothing waits any more. */
  fun reset() {
    pending.value = null
    offer.value = null
  }

  /** Reads the announcement; the mover's own device follows at once, others see the banner. */
  private suspend fun check() {
    val move = checkLock.withLock { offerFor() } ?: return
    if (move.isMine) accept(move.move) else offer.value = move.offer
  }

  private suspend fun offerFor(): CheckedMove? {
    val move = pending.value ?: return null
    if (offer.value != null || accepting.value) return null
    val maybeInfo = try {
      core.core().callMoveOffer(move.from, move.to)
    } catch (error: TeamCraftException) {
      Log.w(TAG, "the call move could not be read", error)
      null
    } catch (error: IllegalStateException) {
      if (error is CancellationException) throw error
      null
    }
    // Not announced in the DM's group yet: asked again on the next change.
    val info = maybeInfo ?: return null
    if (pending.value != move) return null
    val maybeSnapshot = readSnapshot()
    // Only a move this device asked for follows without a tap (not one replayed from earlier,
    // nor the user's own from another device).
    val isMine = info.isRequestedHere
    val offer = CallMoveOffer(
      from = move.from,
      to = move.to,
      byName = maybeSnapshot?.name(info.byUserId) ?: info.byUserId,
      addedNames = info.addedUserIds.map { maybeSnapshot?.name(it) ?: it },
    )
    return CheckedMove(move, offer, isMine)
  }

  private suspend fun accept(move: PendingMove) {
    if (!accepting.compareAndSet(expect = false, update = true)) return
    try {
      // Already followed, or the call ended meanwhile.
      if (activeConversation() != move.from) return
      try {
        core.core().acceptCallMove(move.from, move.to)
      } catch (error: TeamCraftException) {
        // A move that doesn't check out: stay in this call.
        Log.w(TAG, "the call move was refused", error)
        pending.value = null
        offer.value = null
        failure.value = MainViewModel.errorText(error)
        return
      }
      // While `accepting` is set, a repeated `onCallMoved` doesn't follow a second time.
      val hasFollowed = follow(move.from, move.to)
      reset()
      if (!hasFollowed) failure.value = R.string.call_failed
    } finally {
      accepting.value = false
    }
  }

  /** People's display names by user ID, for naming the call's participants. */
  suspend fun names(): Map<String, String> {
    val snapshot = readSnapshot() ?: return emptyMap()
    return (snapshot.users + snapshot.me).associate { it.id to it.displayName }
  }

  private suspend fun readSnapshot(): ChatSnapshot? =
    try {
      withContext(Dispatchers.Default) { chat.snapshot() }
    } catch (error: TeamCraftException) {
      null
    } catch (error: SerializationException) {
      null
    } catch (error: IllegalStateException) {
      if (error is CancellationException) throw error
      null
    }

  private data class PendingMove(val from: String, val to: String)

  private data class CheckedMove(val move: PendingMove, val offer: CallMoveOffer, val isMine: Boolean)

  private companion object {
    const val TAG = "TeamCraftCalls"
  }
}

/** Who can be added to a DM or group DM call. */
data class AddPeopleChoice(val conversationId: String, val myUserId: String, val candidates: List<User>)

/** A move of the call announced in the DM's group, with names for the banner. */
data class CallMoveOffer(val from: String, val to: String, val byName: String, val addedNames: List<String>)

/** The "Add people" request. */
sealed interface AddPeopleState {
  data object Idle : AddPeopleState

  data object Pending : AddPeopleState

  /** The call goes on unchanged; the dialog stays for a retry with the same request key. */
  data class Failed(@StringRes val text: Int) : AddPeopleState

  data object Done : AddPeopleState
}
