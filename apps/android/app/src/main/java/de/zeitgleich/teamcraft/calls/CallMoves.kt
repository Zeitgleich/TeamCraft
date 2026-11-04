package de.zeitgleich.teamcraft.calls

import android.util.Log
import androidx.annotation.StringRes
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.delay
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
import uniffi.teamcraft_ffi.NotetakerCallInfo
import uniffi.teamcraft_ffi.TeamCraftException

/**
 * "Add people" in a DM or group DM call, "Add notetaker" in any call, and following a move of the
 * call into a meeting room (docs/meetings.md, "A call becomes a meeting"; e2ee.md §8.2, "In every
 * call"). The core starts the room, founds its group and checks every move; this class keeps the
 * dialog's and the banners' state. Nothing moves on the
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
  private val notetakerAdding = MutableStateFlow<NotetakerAddState>(NotetakerAddState.Idle)
  private val checkLock = Mutex()

  /** A move another member announced, until joined, refused or the call ended. */
  val moveOffer: StateFlow<CallMoveOffer?> = offer

  /** Where the "Add people" request stands. */
  val addState: StateFlow<AddPeopleState> = adding

  /** Why following a move failed (string resource), until dismissed. */
  val moveFailure: StateFlow<Int?> = failure

  /** The move is being checked and the room's call joined. */
  val isAccepting: StateFlow<Boolean> = accepting

  /** Where "Add notetaker" stands: asked in, it shows as on its way until it is there. */
  val notetakerState: StateFlow<NotetakerAddState> = notetakerAdding

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

  /**
   * The notetaker in the call of `conversationId` with `participantIds` (the others), as the
   * verified policy names it and the call's participants show it; `null` without one.
   */
  suspend fun notetaker(conversationId: String, participantIds: List<String>): NotetakerCallInfo? =
    try {
      core.core().notetakerInCall(conversationId, participantIds)
    } catch (error: TeamCraftException) {
      Log.w(TAG, "the notetaker could not be read", error)
      null
    } catch (error: IllegalStateException) {
      if (error is CancellationException) throw error
      null
    }

  /**
   * "Add notetaker" (e2ee.md §8.2, "In every call"): where it is a member of the conversation,
   * asks it in; anywhere else the call moves into a meeting room with everybody in it and the
   * notetaker, which the core asks in there (it isn't rung), and this device follows. `requestKey`
   * is the call screen's: a retry after a lost answer gets the same room.
   */
  fun addNotetaker(conversationId: String, notetaker: NotetakerCallInfo, title: String, requestKey: String) {
    if (notetakerAdding.value == NotetakerAddState.Pending || !notetaker.canInvite) return
    notetakerAdding.value = NotetakerAddState.Pending
    scope.launch {
      notetakerAdding.value = try {
        if (notetaker.isMember) {
          core.core().askNotetakerIn(conversationId)
        } else {
          val me = (readSnapshot() ?: error("no account")).me.id
          // Who is in the call now (plus the user); the core keeps only members of the group.
          val inCall = (callPeople() + me).distinct()
          val to = core.core().addPeopleToCall(conversationId, inCall, listOf(notetaker.botUserId), title, requestKey)
          moved(conversationId, to)
        }
        NotetakerAddState.Asked
      } catch (error: TeamCraftException) {
        Log.w(TAG, "adding the notetaker failed", error)
        NotetakerAddState.Failed(MainViewModel.errorText(error))
      } catch (error: IllegalStateException) {
        if (error is CancellationException) throw error
        Log.w(TAG, "adding the notetaker failed", error)
        NotetakerAddState.Failed(R.string.error_generic)
      }
      if (notetakerAdding.value == NotetakerAddState.Asked) {
        delay(NOTETAKER_ASKED_MILLIS)
        notetakerAdding.compareAndSet(NotetakerAddState.Asked, NotetakerAddState.Idle)
      }
    }
  }

  /** The notetaker is in the call, or the failure was read: nothing waits any more. */
  fun notetakerShown() {
    if (notetakerAdding.value != NotetakerAddState.Pending) notetakerAdding.value = NotetakerAddState.Idle
  }

  /** "Remove notetaker": sends it away from the call of `conversationId`. */
  fun sendNotetakerAway(conversationId: String) {
    scope.launch {
      try {
        core.core().sendNotetakerAway(conversationId)
      } catch (error: TeamCraftException) {
        Log.w(TAG, "sending the notetaker away failed", error)
      } catch (error: IllegalStateException) {
        if (error is CancellationException) throw error
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
    // "Add notetaker" moved it: the banner says where the audio goes before the tap.
    val maybeBot = notetaker(move.from, emptyList())?.botUserId
    val offer = CallMoveOffer(
      from = move.from,
      to = move.to,
      byName = maybeSnapshot?.name(info.byUserId) ?: info.byUserId,
      addedNames = info.addedUserIds.map { maybeSnapshot?.name(it) ?: it },
      isNotetakerOnly = isNotetakerMove(info.addedUserIds, maybeBot),
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

    /** How long "being added" shows after asking, if the notetaker doesn't come. */
    const val NOTETAKER_ASKED_MILLIS = 90_000L
  }
}

/** Who can be added to a DM or group DM call. */
data class AddPeopleChoice(val conversationId: String, val myUserId: String, val candidates: List<User>)

/**
 * A move of the call announced in the conversation's group, with names for the banner.
 * `isNotetakerOnly`: it added nobody but the notetaker ("Add notetaker").
 */
data class CallMoveOffer(
  val from: String,
  val to: String,
  val byName: String,
  val addedNames: List<String>,
  val isNotetakerOnly: Boolean = false,
)

/** The "Add people" request. */
sealed interface AddPeopleState {
  data object Idle : AddPeopleState

  data object Pending : AddPeopleState

  /** The call goes on unchanged; the dialog stays for a retry with the same request key. */
  data class Failed(@StringRes val text: Int) : AddPeopleState

  data object Done : AddPeopleState
}

/** The move added nobody but the notetaker (`maybeBot`, from the verified policy): "Add notetaker". */
internal fun isNotetakerMove(added: List<String>, maybeBot: String?): Boolean = maybeBot != null && added == listOf(maybeBot)

/** "Add notetaker". */
sealed interface NotetakerAddState {
  data object Idle : NotetakerAddState

  data object Pending : NotetakerAddState

  /** Asked in (or the call moved to it): on its way until it is in the call. */
  data object Asked : NotetakerAddState

  /** The call goes on unchanged. */
  data class Failed(@StringRes val text: Int) : NotetakerAddState
}
