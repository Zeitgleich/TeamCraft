package de.zeitgleich.teamcraft.calls

import android.Manifest
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.graphics.Bitmap
import android.net.Uri
import android.telecom.DisconnectCause
import android.util.Log
import androidx.core.content.ContextCompat
import androidx.core.telecom.CallAttributesCompat
import androidx.core.telecom.CallControlScope
import androidx.core.telecom.CallEndpointCompat
import androidx.core.telecom.CallsManager
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.ProcessLifecycleOwner
import java.nio.ByteBuffer
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Deferred
import kotlinx.coroutines.Job
import kotlinx.coroutines.async
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeoutOrNull
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.CoreRepository
import de.zeitgleich.teamcraft.ui.listName
import uniffi.teamcraft_ffi.CallEndReason
import uniffi.teamcraft_ffi.CallListener
import uniffi.teamcraft_ffi.CallPhase
import uniffi.teamcraft_ffi.ConversationKind
import uniffi.teamcraft_ffi.CallSession
import uniffi.teamcraft_ffi.CallStateInfo
import uniffi.teamcraft_ffi.CameraFrame
import uniffi.teamcraft_ffi.IncomingCallInfo
import uniffi.teamcraft_ffi.TeamCraftException
import uniffi.teamcraft_ffi.VideoFrameRgba
import uniffi.teamcraft_ffi.VideoListener
import uniffi.teamcraft_ffi.VideoTrackInfo

/**
 * Calls through Android's Telecom stack (Jetpack Core-Telecom): the system knows about the call,
 * so ringing, the lock screen, Bluetooth headsets, car kits and Android Auto all work, and a
 * cellular call puts ours on hold. Media and encryption run in the Rust call engine
 * ([CallSession]); this class connects the two and hands the camera's frames to the engine.
 */
class CallController(
  private val context: Context,
  private val core: CoreRepository,
  private val scope: CoroutineScope,
) {
  private val callsManager = CallsManager(context)
  private val state = MutableStateFlow<CallStateInfo?>(null)
  private val wantsCamera = MutableStateFlow(false)
  private val tracks = MutableStateFlow<List<VideoTrackInfo>>(emptyList())
  private val frames = MutableStateFlow<Map<UInt, Bitmap>>(emptyMap())
  private val speaker = MutableStateFlow(false)
  @Volatile private var maybeSession: CallSession? = null
  @Volatile private var maybeTelecomJob: Job? = null
  @Volatile private var maybeTelecomCall: CallControlScope? = null
  @Volatile private var endpoints: List<CallEndpointCompat> = emptyList()
  @Volatile private var isStarted = false
  @Volatile private var isCameraStarting = false
  @Volatile private var maybePermission: CompletableDeferred<Boolean>? = null
  @Volatile private var options = CallOptions()
  @Volatile private var isTelecomRegistered = false
  @Volatile private var maybeRingTimeout: Job? = null
  private val ringing = MutableStateFlow<IncomingRing?>(null)
  private val joining = MutableStateFlow(false)
  /** The engine's join while it connects; hanging up cancels it. */
  @Volatile private var maybePendingJoin: Deferred<CallSession>? = null
  /** Hang up came while joining: a join that finishes anyway leaves at once. */
  @Volatile private var isJoinAbandoned = false
  private val incomingNotifier = IncomingCallNotifier(context)
  /** The DM whose call this device left for a meeting room: its late states are not ours any more. */
  @Volatile private var maybeLeftConversation: String? = null
  private val failure = MutableStateFlow<JoinFailure?>(null)
  /** [CallActivity] is on screen; a failure it can't show goes to a notification. */
  @Volatile private var isCallScreenVisible = false
  /** The call somebody else was in with this device: only then does being alone end it. */
  @Volatile private var hadOthersIn: String? = null
  @Volatile private var maybeAloneEnd: Job? = null

  /** "Add people" and following a move of the call into a meeting room. */
  val moves = CallMoves(
    core,
    scope,
    activeConversation = { if (maybeSession != null) state.value?.conversationId else null },
    callPeople = { state.value?.participantUserIds.orEmpty() },
    follow = ::follow,
  )

  /** The call this device is in, as the engine reports it; `null` when idle. */
  val callState: StateFlow<CallStateInfo?> = state

  /** The user turned the camera on; the call screen captures while it is shown. */
  val isCameraWanted: StateFlow<Boolean> = wantsCamera

  /** Other devices' video tracks in the call. */
  val videoTracks: StateFlow<List<VideoTrackInfo>> = tracks

  /** The newest decoded frame per video track (by stream id). */
  val videoFrames: StateFlow<Map<UInt, Bitmap>> = frames

  /** Audio goes to the loudspeaker. */
  val isSpeakerOn: StateFlow<Boolean> = speaker

  /** The call ringing on this device, until answered, declined or ended elsewhere. */
  val incomingRing: StateFlow<IncomingRing?> = ringing

  /** Joining a call (answered, not connected yet). */
  val isJoining: StateFlow<Boolean> = joining

  /** Why the last join failed, until the user closed it; the call screen shows it. */
  val joinFailure: StateFlow<JoinFailure?> = failure

  /** Registers with Telecom and listens for rings. Safe to call more than once. */
  suspend fun start() {
    if (isStarted) return
    registerWithTelecom()
    core.core().setCallListener(Listener())
    moves.start()
    isStarted = true
  }

  /**
   * Rings for an incoming call (a `call` wake-up, or the gateway while the app runs): the
   * incoming-call notification with its full-screen intent, and a Telecom call so headsets, car
   * kits and Android Auto can answer. Ignored while this device is in a call or another rings.
   */
  fun ring(incoming: IncomingRing) {
    if (maybeSession != null || joining.value) return
    val current = ringing.value
    if (current != null) {
      // The gateway and a wake-up may report the same ring; another conversation's waits.
      val maybeMerged = current.merged(incoming) ?: return
      ringing.value = maybeMerged
      // The device verified the ring only now: offer answering from here on.
      if (maybeMerged.isVerified && !current.isVerified) {
        Log.i(TAG, "ring: verified by a second report")
        incomingNotifier.show(maybeMerged)
        showIncomingIfInFront(maybeMerged)
        reportToTelecom(maybeMerged)
      }
      return
    }
    ringing.value = incoming
    Log.i(TAG, "ring: shown (verified ${incoming.isVerified})")
    incomingNotifier.show(incoming)
    showIncomingIfInFront(incoming)
    maybeRingTimeout?.cancel()
    maybeRingTimeout = scope.launch {
      delay(IncomingCallNotifier.RING_TIMEOUT_MS)
      if (ringing.value?.conversationId == incoming.conversationId) endRing()
    }
    if (incoming.isVerified) reportToTelecom(incoming)
  }

  /**
   * The incoming-call screen over the app when it is in front: the full-screen intent only takes
   * the screen of a locked phone (#1272). In the background the notification rings as before.
   */
  private fun showIncomingIfInFront(incoming: IncomingRing) {
    if (!incoming.opensScreenAtOnce(isAppVisible())) return
    Log.i(TAG, "ring: incoming-call screen opened over the app")
    val intent = Intent(context, CallActivity::class.java)
      .setAction(CallActivity.ACTION_SHOW_INCOMING)
      .putExtra(CallActivity.EXTRA_CONVERSATION_ID, incoming.conversationId)
      .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
    runCatching { context.startActivity(intent) }.onFailure { Log.w(TAG, "ring: incoming-call screen not opened", it) }
  }

  private fun isAppVisible(): Boolean =
    ProcessLifecycleOwner.get().lifecycle.currentState.isAtLeast(Lifecycle.State.STARTED)

  /** Lets headsets, car kits and Android Auto answer; only for rings the device verified. */
  private fun reportToTelecom(incoming: IncomingRing) {
    scope.launch {
      try {
        registerWithTelecom()
        addTelecomCall(incoming.conversationId, incoming.displayName, CallAttributesCompat.DIRECTION_INCOMING)
      } catch (error: Exception) {
        // The notification still rings; only headsets and cars can't answer.
        Log.w(TAG, "the ring could not be reported to Telecom", error)
      }
    }
  }

  /** Answers from the notification or the incoming-call screen. */
  fun answer(conversationId: String) = answer(conversationId, AnswerSource.APP)

  /**
   * Also works when the ring is gone (the process restarted in between): joining needs only the
   * conversation. The join is claimed before the ring is cleared, so the call screen goes straight
   * from ringing to joining (it never sees an idle moment and closes), and a late duplicate ring
   * can't ring again meanwhile. Returns at once ([startAnswer]): Telecom's answer callback must.
   */
  private fun answer(conversationId: String, source: AnswerSource) {
    val maybeCall = maybeTelecomCall
    val route = AnswerRoute.of(
      source,
      isInCall = maybeSession != null,
      isJoining = joining.value,
      hasTelecomCall = maybeCall != null,
      isTelecomAdding = maybeTelecomJob?.isActive == true,
    )
    if (route == AnswerRoute.IGNORE || !joining.compareAndSet(expect = false, update = true)) {
      Log.i(TAG, "answer ($source): ignored, already in or joining a call")
      return
    }
    Log.i(TAG, "answer ($source): $route")
    failure.value = null
    incomingNotifier.cancelJoinFailed()
    clearRing()
    startAnswer(
      scope,
      route,
      object : AnswerSteps {
        override suspend fun answerTelecomCall() {
          runCatching { maybeCall?.answer(CallAttributesCompat.CALL_TYPE_AUDIO_CALL) }
            .onFailure { Log.w(TAG, "Telecom refused the answer", it) }
        }

        override suspend fun join(): Boolean = this@CallController.join(conversationId, isClaimed = true)

        override suspend fun addTelecomCall() {
          options = CallOptions()
          this@CallController.addTelecomCall(conversationId, displayNameOf(conversationId), CallAttributesCompat.DIRECTION_OUTGOING, isJoinClaimed = true)
        }

        // The Telecom call as it is now: one added meanwhile is ended too.
        override suspend fun endCall() = this@CallController.endCall(maybeTelecomCall ?: maybeCall)
      },
    )
  }

  /** Declines on this device: stops ringing. The others keep ringing until they answer or it ends. */
  fun decline() {
    if (maybeSession != null) return
    val maybeCall = maybeTelecomCall
    clearRing()
    if (maybeCall != null) {
      scope.launch {
        runCatching { maybeCall.disconnect(DisconnectCause(DisconnectCause.REJECTED)) }
        maybeTelecomJob?.cancel()
      }
    } else {
      maybeTelecomJob?.cancel()
    }
  }

  /** A `dismiss` wake-up: the ring ended elsewhere (answered, declined, timed out, cancelled). */
  fun stopRinging(callId: String) {
    if (ringing.value?.isEndedBy(callId) == true) endRing()
  }

  /** Starts (or joins) the conversation's call, with the choices of the pre-join dialog. */
  fun startCall(conversationId: String, displayName: String, callOptions: CallOptions = CallOptions()) {
    if (maybeSession != null || joining.value) {
      // Already in (or joining) a call: show it instead of doing nothing.
      Log.i(TAG, "call: already in a call, showing it")
      showCall()
      return
    }
    failure.value = null
    options = callOptions
    addTelecomCall(conversationId, displayName, CallAttributesCompat.DIRECTION_OUTGOING)
  }

  fun setMuted(isMuted: Boolean) {
    maybeSession?.setMuted(isMuted)
  }

  fun setDeafened(isDeafened: Boolean) {
    maybeSession?.setDeafened(isDeafened)
  }

  /** Turns the camera on or off; the call screen asks for the permission and captures. */
  fun setCameraWanted(isWanted: Boolean) {
    wantsCamera.value = isWanted
    if (!isWanted) {
      maybeSession?.stopCamera()
      CallService.start(context, hasCamera = false)
    }
  }

  /**
   * One camera image from the call screen. The first one publishes the camera track with the
   * image's upright size; images before the track runs are dropped.
   */
  fun pushCameraFrame(frame: CameraFrame, uprightWidth: Int, uprightHeight: Int) {
    val session = maybeSession ?: return
    if (session.pushCameraFrame(frame) || isCameraStarting || state.value?.isCameraOn == true) return
    isCameraStarting = true
    scope.launch {
      try {
        session.startCamera(uprightWidth.toUShort(), uprightHeight.toUShort(), CAMERA_FRAMERATE.toUByte())
        CallService.start(context, hasCamera = true)
      } catch (error: Exception) {
        Log.w(TAG, "the camera could not be published", error)
        wantsCamera.value = false
      } finally {
        isCameraStarting = false
      }
    }
  }

  /** Switches between the loudspeaker and the earpiece (or the headset that is connected). */
  fun setSpeakerOn(isOn: Boolean) {
    val call = maybeTelecomCall ?: return
    val target = if (isOn) {
      endpoints.firstOrNull { it.type == CallEndpointCompat.TYPE_SPEAKER }
    } else {
      endpoints.firstOrNull { it.type != CallEndpointCompat.TYPE_SPEAKER }
    } ?: return
    scope.launch { call.requestEndpointChange(target) }
  }

  /**
   * Leaves the call; the engine's ended state then ends the Telecom call and the ongoing-call
   * notification. While still joining it cancels the join; while ringing it declines.
   */
  fun hangUp() {
    val session = maybeSession
    when {
      session != null -> session.leave()
      joining.value -> abandonJoin()
      else -> decline()
    }
  }

  /** Stops a join under way; one that finishes anyway leaves at once. */
  private fun abandonJoin() {
    isJoinAbandoned = true
    maybePendingJoin?.cancel()
    // A join still waiting for the microphone permission gives up too.
    maybePermission?.complete(false)
  }

  /**
   * Adds the call to Telecom. With [isJoinClaimed] the caller (answering) already claimed the join,
   * and gives it up here if the call never gets that far.
   */
  private fun addTelecomCall(conversationId: String, displayName: String, direction: Int, isJoinClaimed: Boolean = false) {
    if (maybeTelecomJob?.isActive == true) {
      Log.w(TAG, "Telecom: the previous call is still being added or ended")
      if (isJoinClaimed) joining.value = false
      return
    }
    // The previous call's ended state would end this one right away (see endWithEngine).
    if (state.value?.phase == CallPhase.ENDED) state.value = null
    val isVideo = direction == CallAttributesCompat.DIRECTION_OUTGOING && options.isCameraOn
    val attributes = CallAttributesCompat(
      displayName = displayName,
      address = Uri.fromParts(URI_SCHEME, conversationId, null),
      direction = direction,
      callType = if (isVideo) CallAttributesCompat.CALL_TYPE_VIDEO_CALL else CallAttributesCompat.CALL_TYPE_AUDIO_CALL,
      callCapabilities = CallAttributesCompat.SUPPORTS_SET_INACTIVE,
    )
    maybeTelecomJob = scope.launch {
      var isClaimPending = isJoinClaimed
      try {
        callsManager.addCall(
          attributes,
          onAnswer = {
            // Answered through Telecom: the system's call UI, a watch, a headset, the car. Telecom
            // waits 5 s for this and tears the call down after; the join runs on its own (#1272).
            Log.i(TAG, "Telecom: answered by the system")
            answer(conversationId, AnswerSource.TELECOM)
          },
          onDisconnect = {
            Log.i(TAG, "Telecom: disconnected by the system")
            if (joining.value) abandonJoin()
            leave()
            maybeTelecomJob?.cancel()
          },
          onSetActive = {},
          onSetInactive = { maybeSession?.setMuted(true) },
        ) {
          maybeTelecomCall = this
          Log.i(TAG, "Telecom: call added")
          if (direction == CallAttributesCompat.DIRECTION_INCOMING && (joining.value || maybeSession != null)) {
            // Answered on the notification while Telecom was still adding the ring: without this the
            // system keeps ringing it, and its missed-call disconnect would end the joined call.
            launch {
              runCatching { this@addCall.answer(CallAttributesCompat.CALL_TYPE_AUDIO_CALL) }
                .onFailure { Log.w(TAG, "Telecom refused the answer", it) }
            }
          }
          if (direction == CallAttributesCompat.DIRECTION_OUTGOING) {
            isClaimPending = false
            launch {
              if (join(conversationId, isJoinClaimed)) setActive() else disconnect(DisconnectCause(DisconnectCause.ERROR))
            }
          }
          launch { endWithEngine(this@addCall) }
          launch { isMuted.collect { maybeSession?.setMuted(it) } }
          launch { availableEndpoints.collect { endpoints = it } }
          launch { currentCallEndpoint.collect { speaker.value = it.type == CallEndpointCompat.TYPE_SPEAKER } }
        }
      } catch (error: Exception) {
        // Also a cancellation: declined, rang out or ended elsewhere.
        if (error !is CancellationException) Log.w(TAG, "the Telecom call failed", error)
        leave()
      } finally {
        // An answer that never got as far as joining gives its claim up.
        if (isClaimPending) joining.value = false
      }
    }
  }

  /**
   * Ends the Telecom call once the engine's call ended (left, removed, ended by the host), and with
   * it this call's job: its collectors never end on their own, and a job still running made the
   * next call's button do nothing.
   */
  private suspend fun endWithEngine(call: CallControlScope) {
    state.first { it?.phase == CallPhase.ENDED }
    call.disconnect(DisconnectCause(DisconnectCause.LOCAL))
    maybeTelecomJob?.cancel()
  }

  /** Called by [CallActivity] with the answer to the microphone permission request. */
  fun onMicrophonePermission(isGranted: Boolean) {
    maybePermission?.complete(isGranted)
  }

  /** Joins in the engine and shows the call; `false` if joining failed. */
  private suspend fun join(conversationId: String, isClaimed: Boolean = false): Boolean {
    if (!isClaimed && (maybeSession != null || !joining.compareAndSet(expect = false, update = true))) {
      return maybeSession != null
    }
    isJoinAbandoned = false
    try {
      return joinOnce(conversationId)
    } finally {
      joining.value = false
    }
  }

  private suspend fun joinOnce(conversationId: String): Boolean {
    if (conversationId == maybeLeftConversation) maybeLeftConversation = null
    showCall()
    if (!hasMicrophone() && !requestMicrophone()) {
      Log.i(TAG, "join: no microphone")
      if (!isJoinAbandoned) report(JoinFailure.MICROPHONE)
      return false
    }
    // Receiving video needs Cisco's OpenH264; without it the call is audio-only for this device.
    VideoCodecInstaller.ensure(context)
    AudioContext.ensure(context)
    if (isJoinAbandoned) return false
    // Muted from the first frame (a muted call moving into a meeting room): no audio slips out.
    val isMuted = options.isMuted
    Log.i(TAG, "join: connecting")
    val pending = scope.async { core.core().joinCall(conversationId, isMuted) }
    maybePendingJoin = pending
    val session = try {
      pending.await()
    } catch (error: Exception) {
      if (error !is CancellationException && !isJoinAbandoned) {
        Log.w(TAG, "join: failed (${error.javaClass.simpleName})")
        report(joinFailureOf(error))
      }
      return false
    } finally {
      maybePendingJoin = null
    }
    if (isJoinAbandoned) {
      // Hung up while it connected.
      session.leave()
      session.close()
      return false
    }
    Log.i(TAG, "join: connected")
    maybeSession = session
    session.setStatusSounds(CallSoundPrefs.prefs(context).statusSounds)
    session.setVideoListener(Video())
    wantsCamera.value = options.isCameraOn
    options = CallOptions()
    CallService.start(context, hasCamera = false)
    return true
  }

  /**
   * Follows a move the core accepted: leaves the DM's call and joins the meeting room's in the
   * same Telecom call, with the microphone and camera as they were. If joining fails, the call
   * ends as failed (and with it the Telecom call).
   */
  private suspend fun follow(from: String, to: String): Boolean {
    val current = state.value
    val session = maybeSession
    if (session == null || current?.conversationId != from) return false
    options = CallOptions(isCameraOn = wantsCamera.value, isMuted = current.isMuted)
    maybeLeftConversation = from
    maybeSession = null
    session.leave()
    session.close()
    tracks.value = emptyList()
    frames.value = emptyMap()
    state.value = current.copy(
      conversationId = to,
      phase = CallPhase.CONNECTING,
      isSecured = false,
      participantCount = 0u,
      participantUserIds = emptyList(),
      peers = emptyList(),
      speakingUserIds = emptyList(),
      mutedUserIds = emptyList(),
      reconnectingCount = 0u,
      reconnectingUserIds = emptyList(),
      maybePrivacyCode = null,
    )
    if (join(to)) return true
    state.update { it?.copy(phase = CallPhase.ENDED, maybeEndReason = CallEndReason.FAILED) }
    endCall(maybeTelecomCall)
    return false
  }

  /** Ends the ring here: notification, timeout and the Telecom call unless it was answered. */
  private fun endRing() {
    clearRing()
    if (maybeSession == null && !joining.value) maybeTelecomJob?.cancel()
  }

  private fun clearRing() {
    ringing.value = null
    maybeRingTimeout?.cancel()
    maybeRingTimeout = null
    incomingNotifier.cancel()
  }

  private fun registerWithTelecom() {
    if (isTelecomRegistered) return
    callsManager.registerAppWithTelecom(CallsManager.CAPABILITY_BASELINE or CallsManager.CAPABILITY_SUPPORTS_VIDEO_CALLING)
    isTelecomRegistered = true
  }

  /** The name the call screen shows for a conversation. */
  suspend fun displayName(conversationId: String): String = displayNameOf(conversationId)

  private suspend fun displayNameOf(conversationId: String): String =
    conversationNameOf(conversationId) ?: context.getString(R.string.app_name)

  /**
   * Ends a one-to-one call shortly after the other person hung up ([isLeftAlone]); somebody coming
   * back in time keeps it.
   */
  private fun followOthers(state: CallStateInfo) {
    if (state.phase != CallPhase.CONNECTED) return
    val count = state.participantCount.toInt()
    if (count > 0) {
      hadOthersIn = state.conversationId
      maybeAloneEnd?.cancel()
      maybeAloneEnd = null
      return
    }
    if (hadOthersIn != state.conversationId || maybeAloneEnd?.isActive == true) return
    maybeAloneEnd = scope.launch {
      val isOneToOne = runCatching {
        core.conversations().firstOrNull { it.id == state.conversationId }?.kind == ConversationKind.DIRECT
      }.getOrDefault(false)
      if (!isOneToOne) return@launch
      delay(ALONE_END_MS)
      val now = this@CallController.state.value ?: return@launch
      val hadOthers = hadOthersIn == now.conversationId
      if (isLeftAlone(isOneToOne, hadOthers, now.participantCount.toInt(), now.reconnectingCount.toInt())) {
        Log.i(TAG, "the other person left: ending the call")
        hangUp()
      }
    }
  }

  /** The name of a conversation this device knows (the notes by their title); `null` for one it doesn't. */
  private suspend fun conversationNameOf(conversationId: String): String? =
    runCatching {
      core.conversations().firstOrNull { it.id == conversationId }?.listName(context.getString(R.string.notes_title))
    }.getOrNull()

  /** The call screen of the running call, from a "Back to the call" outside it. */
  fun bringToFront() {
    if (maybeSession != null || joining.value) showCall()
  }

  private fun showCall() {
    // The previous call's ended state would close the screen right away.
    if (state.value?.phase == CallPhase.ENDED) state.value = null
    // Android may refuse this from the background (answered on a watch or in the car); the
    // ongoing-call notification opens the screen then.
    Log.i(TAG, "join: call screen asked for")
    runCatching { context.startActivity(Intent(context, CallActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)) }
      .onFailure { Log.w(TAG, "join: call screen not opened", it) }
  }

  /** [CallActivity] came on screen or left it. */
  fun onCallScreenVisible(isVisible: Boolean) {
    isCallScreenVisible = isVisible
  }

  /** The user closed the failure the call screen showed. */
  fun dismissJoinFailure() {
    failure.value = null
  }

  /** A failed join: the call screen shows why, or a notification when the screen isn't up. */
  private fun report(joinFailure: JoinFailure) {
    failure.value = joinFailure
    if (!isCallScreenVisible) incomingNotifier.showJoinFailed(joinFailure)
  }

  private fun joinFailureOf(error: Exception): JoinFailure = when (error) {
    is TeamCraftException.Network -> JoinFailure.NETWORK
    is TeamCraftException.CallDeviceLimit -> JoinFailure.DEVICE_LIMIT
    is TeamCraftException.CallEncryptionPending -> JoinFailure.ENCRYPTION_PENDING
    else -> JoinFailure.OTHER
  }

  private fun hasMicrophone(): Boolean =
    ContextCompat.checkSelfPermission(context, Manifest.permission.RECORD_AUDIO) ==
      PackageManager.PERMISSION_GRANTED

  /**
   * [CallActivity] asks the user; waits for the answer, at most [MICROPHONE_WAIT_MS]: a screen that
   * could not open (answered from the background) or an answer that came before this waits would
   * otherwise keep the join, and every later ring, stuck.
   */
  private suspend fun requestMicrophone(): Boolean {
    val answer = CompletableDeferred<Boolean>()
    maybePermission = answer
    return try {
      // Granted before this started waiting.
      if (hasMicrophone()) return true
      withTimeoutOrNull(MICROPHONE_WAIT_MS) { answer.await() } ?: false
    } finally {
      maybePermission = null
    }
  }

  /**
   * Ends a call that didn't (or no longer) runs in the engine: the Telecom call if there is one,
   * and everything [leave] cleans up, also when there is no Telecom call to end.
   */
  private suspend fun endCall(maybeCall: CallControlScope?) {
    if (maybeCall != null) runCatching { maybeCall.disconnect(DisconnectCause(DisconnectCause.ERROR)) }
    maybeTelecomJob?.cancel()
    leave()
  }

  /** Cleans up after a call; safe to call more than once. */
  private fun leave() {
    maybeSession?.let { session ->
      session.leave()
      session.close()
    }
    maybeSession = null
    maybeTelecomCall = null
    moves.reset()
    wantsCamera.value = false
    tracks.value = emptyList()
    frames.value = emptyMap()
    CallService.stop(context)
  }

  /** Engine callbacks arrive on the engine's and the call's threads; the state flow is thread-safe. */
  private inner class Listener : CallListener {
    override fun onCallState(state: CallStateInfo) {
      // The DM's call this device left for a meeting room ends after the room's call began.
      if (state.conversationId == maybeLeftConversation) return
      this@CallController.state.value = state
      followOthers(state)
      // The engine is the source of truth: once its call ended, the session, the service and its
      // notification go too, whether or not Telecom reports its disconnect back (endWithEngine
      // ends the Telecom call). Not on the engine's thread, which reports this.
      if (state.phase == CallPhase.ENDED && maybeSession != null) scope.launch { leave() }
    }

    override fun onIncomingCall(call: IncomingCallInfo) {
      scope.launch {
        val maybeName = conversationNameOf(call.conversationId)
        ring(
          IncomingRing(
            call.conversationId,
            maybeCallId = null,
            displayName = maybeName ?: context.getString(R.string.app_name),
            maybeCallerName = null,
            isVerified = maybeName != null,
          ),
        )
      }
    }

    override fun onRingEnded(conversationId: String) {
      // Answered on another device, declined or over: stop ringing here.
      if (ringing.value?.conversationId == conversationId) endRing()
    }

    override fun onCallMoved(fromConversationId: String, toConversationId: String) {
      // Only a trigger: the move is shown, and followed, once the DM's group announced it.
      moves.moved(fromConversationId, toConversationId)
    }
  }

  /** Other devices' video, on the call's and the decoders' threads. */
  private inner class Video : VideoListener {
    override fun onVideoTracks(tracks: List<VideoTrackInfo>) {
      this@CallController.tracks.value = tracks
      val live = tracks.map { it.streamId }.toSet()
      frames.update { current -> current.filterKeys { it in live } }
    }

    override fun onVideoFrame(frame: VideoFrameRgba) {
      val bitmap = Bitmap.createBitmap(frame.width.toInt(), frame.height.toInt(), Bitmap.Config.ARGB_8888)
      // ARGB_8888 is stored as R, G, B, A bytes: the engine's RGBA order.
      bitmap.copyPixelsFromBuffer(ByteBuffer.wrap(frame.rgba))
      frames.update { it + (frame.streamId to bitmap) }
    }
  }

  companion object {
    private const val TAG = "TeamCraftCalls"
    /** Frames per second the camera sends. */
    const val CAMERA_FRAMERATE = 15
    /** Telecom needs an address per call; ours names the conversation. */
    const val URI_SCHEME = "teamcraft"
    /** How long a join waits for the user to answer the microphone permission request. */
    private const val MICROPHONE_WAIT_MS = 60_000L
  }
}

/** What the user chose before joining. */
data class CallOptions(val isCameraOn: Boolean = false, val isMuted: Boolean = false)
