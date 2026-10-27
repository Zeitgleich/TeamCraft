package de.zeitgleich.teamcraft.ui

import androidx.annotation.StringRes
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.CoreRepository
import uniffi.teamcraft_ffi.JoinHistory
import uniffi.teamcraft_ffi.JoinRequestItem
import uniffi.teamcraft_ffi.JoinRequestOutcome
import uniffi.teamcraft_ffi.TeamCraftException

/**
 * Admins asking to join a private channel under `admin_self_join = owner_approval` (admin.md
 * §2.4.1, issue #799). The core decides everything that matters: which requests wait for this
 * user, whether Approve is offered (only for an admin of the fresh verified roster), and approving
 * is its own signed MLS add. The screens show the list and wait for the one that runs.
 */
class JoinRequestActions(
  private val core: CoreRepository,
  private val scope: CoroutineScope,
  private val onFailure: (Int) -> Unit,
) {
  private val running = MutableStateFlow<Set<String>>(emptySet())

  /** Requests (by [key]) and channels (by ID) whose action runs: their buttons wait meanwhile. */
  val pending: StateFlow<Set<String>> = running

  /** The requests waiting for this user's decision, oldest first; empty when they can't be read. */
  suspend fun list(): List<JoinRequestItem> =
    try {
      core.core().joinRequests()
    } catch (failure: CancellationException) {
      throw failure
    } catch (failure: Exception) {
      // Not enrolled, signed out meanwhile or the store is gone: nothing to decide here.
      emptyList()
    }

  /** Lets the admin in, sharing [history]: the earlier history only when the owner ticked it. */
  fun approve(request: JoinRequestItem, history: JoinHistory) =
    run(key(request), JoinStep.Approve, request.conversationId, request.userId, history)

  fun decline(request: JoinRequestItem) = run(key(request), JoinStep.Decline, request.conversationId, request.userId)

  /** "Ask to join" in the channel browser. */
  fun ask(conversationId: String) = run(conversationId, JoinStep.Ask, conversationId, null)

  fun withdraw(conversationId: String) = run(conversationId, JoinStep.Withdraw, conversationId, null)

  private fun run(
    pendingKey: String,
    step: JoinStep,
    conversationId: String,
    maybeUser: String?,
    history: JoinHistory = JoinHistory.FROM_JOIN,
  ) {
    if (pendingKey in running.value) return
    running.update { it + pendingKey }
    scope.launch {
      try {
        joinOutcomeMessage(step, step.run(core, conversationId, maybeUser, history))?.let(onFailure)
      } catch (failure: TeamCraftException) {
        onFailure(MainViewModel.errorText(failure))
      } catch (failure: CancellationException) {
        throw failure
      } catch (failure: IllegalStateException) {
        // Signed out meanwhile: no account to act for.
      } finally {
        running.update { it - pendingKey }
      }
    }
  }

  companion object {
    /** One request: an admin and a channel. */
    fun key(request: JoinRequestItem): String = "${request.conversationId}:${request.userId}"
  }
}

/** One action on a request, with the core call it makes. */
enum class JoinStep {
  Ask,
  Withdraw,
  Approve,
  Decline;

  /**
   * Runs on the core of the signed-in account; `user` only for the owner's decisions. Approving
   * shares [history]: what a self-join gets unless the owner explicitly chose the earlier history.
   */
  suspend fun run(
    core: CoreRepository,
    conversationId: String,
    user: String?,
    history: JoinHistory = JoinHistory.FROM_JOIN,
  ): JoinRequestOutcome {
    val client = core.core()
    return when (this) {
      Ask -> client.requestAdminJoin(conversationId)
      Withdraw -> client.withdrawAdminJoin(conversationId)
      Approve -> client.approveAdminJoin(conversationId, requireNotNull(user), history)
      Decline -> client.declineAdminJoin(conversationId, requireNotNull(user))
    }
  }
}

/** What to tell the user when [step] changed nothing; `null` when it went through. */
@StringRes
fun joinOutcomeMessage(step: JoinStep, outcome: JoinRequestOutcome): Int? = when (outcome) {
  JoinRequestOutcome.DONE -> null
  JoinRequestOutcome.GONE -> R.string.join_request_gone
  // Asking: this user's own role isn't in a fresh signed roster; approving: the requester's isn't.
  JoinRequestOutcome.NOT_AN_ADMIN -> if (step == JoinStep.Ask) R.string.join_ask_not_admin else R.string.join_request_not_admin
  JoinRequestOutcome.TOO_SOON -> R.string.join_ask_too_soon
  JoinRequestOutcome.NOT_APPROVABLE -> R.string.join_request_not_approvable
}

/** The requests for one channel, for its conversation screen. */
fun requestsIn(conversationId: String, requests: List<JoinRequestItem>): List<JoinRequestItem> =
  requests.filter { it.conversationId == conversationId }

/** How the channel browser offers a channel the user is not in (the core's `BrowseJoin`). */
enum class BrowseJoin {
  /** A public channel: anyone in the space joins. */
  Open,

  /** A private channel a workspace admin joins by themselves; that happens on desktop and web. */
  AsAdmin,

  /** A private channel whose owners decide: "Ask to join". */
  NeedsApproval,

  /** Asked already: "Requested" with "Withdraw". */
  Requested;

  companion object {
    /** The core's wire value; one this version doesn't know offers nothing to press. */
    fun of(wire: String): BrowseJoin? = when (wire) {
      "open" -> Open
      "as_admin" -> AsAdmin
      "needs_approval" -> NeedsApproval
      "requested" -> Requested
      else -> null
    }
  }
}
