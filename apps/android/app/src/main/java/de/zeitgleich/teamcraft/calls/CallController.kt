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
import java.nio.ByteBuffer
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.CoreRepository
import uniffi.teamcraft_ffi.CallListener
import uniffi.teamcraft_ffi.CallPhase
import uniffi.teamcraft_ffi.CallSession
import uniffi.teamcraft_ffi.CallStateInfo
import uniffi.teamcraft_ffi.CameraFrame
import uniffi.teamcraft_ffi.IncomingCallInfo
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
  @Volatile private var maybeNetworkWatcher: NetworkWatcher? = null
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
  private val incomingNotifier = IncomingCallNotifier(context)

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

  /** Registers with Telecom and listens for rings. Safe to call more than once. */
  suspend fun start() {
    if (isStarted) return
    registerWithTelecom()
    core.core().setCallListener(Listener())
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
        incomingNotifier.show(maybeMerged)
        reportToTelecom(maybeMerged)
      }
      return
    }
    ringing.value = incoming
    incomingNotifier.show(incoming)
    maybeRingTimeout?.cancel()
    maybeRingTimeout = scope.launch {
      delay(IncomingCallNotifier.RING_TIMEOUT_MS)
      if (ringing.value?.conversationId == incoming.conversationId) endRing()
    }
    if (incoming.isVerified) reportToTelecom(incoming)
  }

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

  /**
   * Answers from the notification or the call screen. Also works when the ring is gone (the
   * process restarted in between): joining needs only the conversation.
   */
  fun answer(conversationId: String) {
    val maybeCall = maybeTelecomCall
    clearRing()
    scope.launch {
      if (maybeCall != null) {
        runCatching { maybeCall.answer(CallAttributesCompat.CALL_TYPE_AUDIO_CALL) }
          .onFailure { Log.w(TAG, "Telecom refused the answer", it) }
        if (!join(conversationId)) maybeCall.disconnect(DisconnectCause(DisconnectCause.ERROR))
      } else if (maybeTelecomJob?.isActive == true) {
        // Telecom is still adding the ring: join without waiting for it.
        join(conversationId)
      } else {
        options = CallOptions()
        addTelecomCall(conversationId, displayNameOf(conversationId), CallAttributesCompat.DIRECTION_OUTGOING)
      }
    }
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

  /** Leaves the call; the engine's ended state then ends the Telecom call. */
  fun hangUp() {
    val session = maybeSession
    if (session == null) {
      clearRing()
      maybeTelecomJob?.cancel()
    } else {
      session.leave()
    }
  }

  private fun addTelecomCall(conversationId: String, displayName: String, direction: Int) {
    if (maybeTelecomJob?.isActive == true) return
    val isVideo = direction == CallAttributesCompat.DIRECTION_OUTGOING && options.isCameraOn
    val attributes = CallAttributesCompat(
      displayName = displayName,
      address = Uri.fromParts(URI_SCHEME, conversationId, null),
      direction = direction,
      callType = if (isVideo) CallAttributesCompat.CALL_TYPE_VIDEO_CALL else CallAttributesCompat.CALL_TYPE_AUDIO_CALL,
      callCapabilities = CallAttributesCompat.SUPPORTS_SET_INACTIVE,
    )
    maybeTelecomJob = scope.launch {
      try {
        callsManager.addCall(
          attributes,
          onAnswer = {
            // Answered through Telecom: a headset, the car or Android Auto.
            clearRing()
            join(conversationId)
          },
          onDisconnect = { leave() },
          onSetActive = {},
          onSetInactive = { maybeSession?.setMuted(true) },
        ) {
          maybeTelecomCall = this
          if (direction == CallAttributesCompat.DIRECTION_OUTGOING) {
            launch {
              if (join(conversationId)) setActive() else disconnect(DisconnectCause(DisconnectCause.ERROR))
            }
          }
          launch { endWithEngine(this@addCall) }
          launch { isMuted.collect { maybeSession?.setMuted(it) } }
          launch { availableEndpoints.collect { endpoints = it } }
          launch { currentCallEndpoint.collect { speaker.value = it.type == CallEndpointCompat.TYPE_SPEAKER } }
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
    if (maybeSession != null || !joining.compareAndSet(expect = false, update = true)) return maybeSession != null
    try {
      return joinOnce(conversationId)
    } finally {
      joining.value = false
    }
  }

  private suspend fun joinOnce(conversationId: String): Boolean {
    showCall()
    if (!hasMicrophone() && !requestMicrophone()) return false
    // Receiving video needs Cisco's OpenH264; without it the call is audio-only for this device.
    VideoCodecInstaller.ensure(context)
    val session = try {
      core.core().joinCall(conversationId)
    } catch (error: Exception) {
      Log.w(TAG, "joining the call failed", error)
      return false
    }
    maybeSession = session
    maybeNetworkWatcher = NetworkWatcher(context) { maybeSession?.networkChanged() }.also { it.start() }
    session.setVideoListener(Video())
    if (options.isMuted) session.setMuted(true)
    wantsCamera.value = options.isCameraOn
    options = CallOptions()
    CallService.start(context, hasCamera = false)
    return true
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

  private suspend fun displayNameOf(conversationId: String): String =
    conversationNameOf(conversationId) ?: context.getString(R.string.app_name)

  /** The name of a conversation this device knows; `null` for one it doesn't. */
  private suspend fun conversationNameOf(conversationId: String): String? =
    runCatching { core.core().conversations().firstOrNull { it.id == conversationId }?.name }.getOrNull()

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
    maybeNetworkWatcher?.stop()
    maybeNetworkWatcher = null
    maybeSession?.let { session ->
      session.leave()
      session.close()
    }
    maybeSession = null
    maybeTelecomCall = null
    wantsCamera.value = false
    tracks.value = emptyList()
    frames.value = emptyMap()
    CallService.stop(context)
  }

  /** Engine callbacks arrive on the engine's and the call's threads; the state flow is thread-safe. */
  private inner class Listener : CallListener {
    override fun onCallState(state: CallStateInfo) {
      this@CallController.state.value = state
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
  }
}

/** What the user chose before joining. */
data class CallOptions(val isCameraOn: Boolean = false, val isMuted: Boolean = false)
