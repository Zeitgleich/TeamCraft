package de.zeitgleich.teamcraft.calls

import android.Manifest
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.telecom.DisconnectCause
import android.util.Log
import androidx.core.content.ContextCompat
import androidx.core.telecom.CallAttributesCompat
import androidx.core.telecom.CallControlScope
import androidx.core.telecom.CallsManager
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.CoreRepository
import uniffi.teamcraft_ffi.CallListener
import uniffi.teamcraft_ffi.CallPhase
import uniffi.teamcraft_ffi.CallSession
import uniffi.teamcraft_ffi.CallStateInfo
import uniffi.teamcraft_ffi.IncomingCallInfo

/**
 * Calls through Android's Telecom stack (Jetpack Core-Telecom): the system knows about the call,
 * so ringing, the lock screen, Bluetooth headsets, car kits and Android Auto all work, and a
 * cellular call puts ours on hold. Media and encryption run in the Rust call engine
 * ([CallSession]); this class connects the two.
 */
class CallController(
  private val context: Context,
  private val core: CoreRepository,
  private val scope: CoroutineScope,
) {
  private val callsManager = CallsManager(context)
  private val state = MutableStateFlow<CallStateInfo?>(null)
  @Volatile private var maybeSession: CallSession? = null
  @Volatile private var maybeTelecomJob: Job? = null
  @Volatile private var isStarted = false
  @Volatile private var maybePermission: CompletableDeferred<Boolean>? = null

  /** The call this device is in, as the engine reports it; `null` when idle. */
  val callState: StateFlow<CallStateInfo?> = state

  /** Registers with Telecom and listens for rings. Safe to call more than once. */
  suspend fun start() {
    if (isStarted) return
    callsManager.registerAppWithTelecom(CallsManager.CAPABILITY_BASELINE)
    core.core().setCallListener(Listener())
    isStarted = true
  }

  /** Starts (or joins) the conversation's call. */
  fun startCall(conversationId: String, displayName: String) {
    addTelecomCall(conversationId, displayName, CallAttributesCompat.DIRECTION_OUTGOING)
  }

  fun setMuted(isMuted: Boolean) {
    maybeSession?.setMuted(isMuted)
  }

  /** Leaves the call; the engine's ended state then ends the Telecom call. */
  fun hangUp() {
    val session = maybeSession
    if (session == null) {
      maybeTelecomJob?.cancel()
    } else {
      session.leave()
    }
  }

  private fun addTelecomCall(conversationId: String, displayName: String, direction: Int) {
    if (maybeTelecomJob?.isActive == true) return
    val attributes = CallAttributesCompat(
      displayName = displayName,
      address = Uri.fromParts(URI_SCHEME, conversationId, null),
      direction = direction,
      callType = CallAttributesCompat.CALL_TYPE_AUDIO_CALL,
      callCapabilities = CallAttributesCompat.SUPPORTS_SET_INACTIVE,
    )
    maybeTelecomJob = scope.launch {
      try {
        callsManager.addCall(
          attributes,
          onAnswer = { join(conversationId) },
          onDisconnect = { leave() },
          onSetActive = {},
          onSetInactive = { maybeSession?.setMuted(true) },
        ) {
          if (direction == CallAttributesCompat.DIRECTION_OUTGOING) {
            launch {
              if (join(conversationId)) setActive() else disconnect(DisconnectCause(DisconnectCause.ERROR))
            }
          }
          launch { endWithEngine(this@addCall) }
          launch { isMuted.collect { maybeSession?.setMuted(it) } }
        }
      } catch (error: Exception) {
        Log.w(TAG, "the Telecom call failed", error)
        leave()
      }
    }
  }

  /** Ends the Telecom call once the engine's call ended (left, removed, ended by the host). */
  private suspend fun endWithEngine(call: CallControlScope) {
    state.first { it?.phase == CallPhase.ENDED }
    call.disconnect(DisconnectCause(DisconnectCause.LOCAL))
  }

  /** Called by [CallActivity] with the answer to the microphone permission request. */
  fun onMicrophonePermission(isGranted: Boolean) {
    maybePermission?.complete(isGranted)
  }

  /** Joins in the engine and shows the call; `false` if joining failed. */
  private suspend fun join(conversationId: String): Boolean {
    showCall()
    if (!hasMicrophone() && !requestMicrophone()) return false
    val session = try {
      core.core().joinCall(conversationId)
    } catch (error: Exception) {
      Log.w(TAG, "joining the call failed", error)
      return false
    }
    maybeSession = session
    CallService.start(context)
    return true
  }

  private fun showCall() {
    // The previous call's ended state would close the screen right away.
    if (state.value?.phase == CallPhase.ENDED) state.value = null
    context.startActivity(Intent(context, CallActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
  }

  private fun hasMicrophone(): Boolean =
    ContextCompat.checkSelfPermission(context, Manifest.permission.RECORD_AUDIO) ==
      PackageManager.PERMISSION_GRANTED

  /** [CallActivity] asks the user (it is on screen by now); waits for the answer. */
  private suspend fun requestMicrophone(): Boolean {
    val answer = CompletableDeferred<Boolean>()
    maybePermission = answer
    return try {
      answer.await()
    } finally {
      maybePermission = null
    }
  }

  private fun leave() {
    maybeSession?.let { session ->
      session.leave()
      session.close()
    }
    maybeSession = null
    CallService.stop(context)
  }

  /** Engine callbacks arrive on the engine's and the call's threads; the state flow is thread-safe. */
  private inner class Listener : CallListener {
    override fun onCallState(state: CallStateInfo) {
      this@CallController.state.value = state
    }

    override fun onIncomingCall(call: IncomingCallInfo) {
      scope.launch {
        val name = core.core().conversations().firstOrNull { it.id == call.conversationId }?.name
        val displayName = name ?: context.getString(R.string.app_name)
        addTelecomCall(call.conversationId, displayName, CallAttributesCompat.DIRECTION_INCOMING)
      }
    }

    override fun onRingEnded(conversationId: String) {
      // Answered on another device, declined or over: stop ringing here.
      if (maybeSession == null) maybeTelecomJob?.cancel()
    }
  }

  companion object {
    private const val TAG = "TeamCraftCalls"
    /** Telecom needs an address per call; ours names the conversation. */
    const val URI_SCHEME = "teamcraft"
  }
}
