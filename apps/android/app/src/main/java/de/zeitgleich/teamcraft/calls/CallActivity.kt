package de.zeitgleich.teamcraft.calls

import android.Manifest
import android.content.pm.PackageManager
import android.graphics.Bitmap
import android.os.Bundle
import android.view.WindowManager.LayoutParams.FLAG_SECURE
import androidx.activity.ComponentActivity
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.items
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.FilterChip
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.TeamCraftApplication
import de.zeitgleich.teamcraft.security.ScreenPrefs.Companion.prefs
import de.zeitgleich.teamcraft.ui.theme.TeamCraftTheme
import uniffi.teamcraft_ffi.CallEndReason
import uniffi.teamcraft_ffi.CallPhase
import uniffi.teamcraft_ffi.CallStateInfo
import uniffi.teamcraft_ffi.VideoTrackInfo

private val SELF_VIEW_WIDTH = 120.dp
private const val SELF_VIEW_ASPECT = 3f / 4f
private const val VIDEO_ASPECT = 4f / 3f

/** The in-call screen: others' video, the self-view, the privacy code and the call's controls. */
class CallActivity : ComponentActivity() {
  private val calls by lazy { TeamCraftApplication.graph(this).calls }

  private val microphonePermission = registerForActivityResult(ActivityResultContracts.RequestPermission()) {
    calls.onMicrophonePermission(it)
  }

  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    enableEdgeToEdge()
    if (prefs(this).isSecureScreen) window.setFlags(FLAG_SECURE, FLAG_SECURE)
    setShowWhenLocked(true)
    setTurnScreenOn(true)
    val microphone = ContextCompat.checkSelfPermission(this, Manifest.permission.RECORD_AUDIO)
    if (microphone != PackageManager.PERMISSION_GRANTED) {
      microphonePermission.launch(Manifest.permission.RECORD_AUDIO)
    }
    setContent {
      TeamCraftTheme {
        val maybeState by calls.callState.collectAsStateWithLifecycle()
        LaunchedEffect(maybeState?.phase) {
          if (maybeState?.phase == CallPhase.ENDED) finish()
        }
        CallScreen(calls, maybeState)
      }
    }
  }
}

@Composable
private fun CallScreen(calls: CallController, maybeState: CallStateInfo?) {
  val tracks by calls.videoTracks.collectAsStateWithLifecycle()
  val frames by calls.videoFrames.collectAsStateWithLifecycle()
  val isCameraWanted by calls.isCameraWanted.collectAsStateWithLifecycle()
  val isSpeakerOn by calls.isSpeakerOn.collectAsStateWithLifecycle()
  var isFrontCamera by rememberSaveable { mutableStateOf(true) }
  val context = LocalContext.current
  var hasCamera by remember {
    mutableStateOf(ContextCompat.checkSelfPermission(context, Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED)
  }
  val cameraPermission = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { isGranted ->
    hasCamera = isGranted
    calls.setCameraWanted(isGranted)
  }
  Surface(modifier = Modifier.fillMaxSize()) {
    Box(Modifier.fillMaxSize().safeDrawingPadding()) {
      Column(
        modifier = Modifier.fillMaxSize().padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
      ) {
        CallStatus(maybeState)
        if (tracks.isEmpty()) {
          Box(Modifier.weight(1f))
        } else {
          VideoGrid(tracks, frames, Modifier.weight(1f))
        }
        CallControls(
          maybeState = maybeState,
          isCameraOn = isCameraWanted,
          isSpeakerOn = isSpeakerOn,
          onMute = { calls.setMuted(it) },
          onDeafen = { calls.setDeafened(it) },
          onCamera = { isOn ->
            if (isOn && !hasCamera) cameraPermission.launch(Manifest.permission.CAMERA) else calls.setCameraWanted(isOn)
          },
          onFlipCamera = { isFrontCamera = !isFrontCamera },
          onSpeaker = { calls.setSpeakerOn(it) },
          onHangUp = { calls.hangUp() },
        )
      }
      if (isCameraWanted && hasCamera) {
        CameraFeed(
          calls = calls,
          isFrontCamera = isFrontCamera,
          modifier = Modifier
            .align(Alignment.TopEnd)
            .padding(16.dp)
            .size(width = SELF_VIEW_WIDTH, height = SELF_VIEW_WIDTH / SELF_VIEW_ASPECT),
        )
      }
    }
  }
}

@Composable
private fun CallStatus(maybeState: CallStateInfo?) {
  Text(stringResource(phaseText(maybeState)), style = MaterialTheme.typography.headlineSmall)
  maybeState?.let { state ->
    if (state.phase == CallPhase.CONNECTED) {
      val count = state.participantCount.toInt()
      Text(pluralStringResource(R.plurals.call_participants, count, count))
    }
    state.maybePrivacyCode?.let { code ->
      Text(stringResource(R.string.call_privacy_code_hint), style = MaterialTheme.typography.bodySmall)
      Text(code, style = MaterialTheme.typography.titleMedium)
    }
  }
}

@Composable
private fun VideoGrid(tracks: List<VideoTrackInfo>, frames: Map<UInt, Bitmap>, modifier: Modifier) {
  val columns = if (tracks.size > 1) 2 else 1
  LazyVerticalGrid(
    columns = GridCells.Fixed(columns),
    modifier = modifier.fillMaxWidth(),
    horizontalArrangement = Arrangement.spacedBy(8.dp),
    verticalArrangement = Arrangement.spacedBy(8.dp),
  ) {
    items(tracks, key = { it.streamId.toLong() }) { track ->
      val maybeFrame = frames[track.streamId]
      Box(
        Modifier.fillMaxWidth().aspectRatio(VIDEO_ASPECT).background(Color.Black),
        contentAlignment = Alignment.Center,
      ) {
        if (maybeFrame == null) {
          Text(stringResource(R.string.call_video_waiting), color = Color.White)
        } else {
          Image(
            bitmap = maybeFrame.asImageBitmap(),
            contentDescription = stringResource(
              if (track.isScreen) R.string.call_video_screen else R.string.call_video_camera,
            ),
            contentScale = ContentScale.Fit,
            modifier = Modifier.fillMaxSize(),
          )
        }
      }
    }
  }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun CallControls(
  maybeState: CallStateInfo?,
  isCameraOn: Boolean,
  isSpeakerOn: Boolean,
  onMute: (Boolean) -> Unit,
  onDeafen: (Boolean) -> Unit,
  onCamera: (Boolean) -> Unit,
  onFlipCamera: () -> Unit,
  onSpeaker: (Boolean) -> Unit,
  onHangUp: () -> Unit,
) {
  val isMuted = maybeState?.isMuted == true
  val isDeafened = maybeState?.isDeafened == true
  FlowRow(
    horizontalArrangement = Arrangement.spacedBy(8.dp, Alignment.CenterHorizontally),
    verticalArrangement = Arrangement.spacedBy(8.dp),
    modifier = Modifier.fillMaxWidth(),
  ) {
    FilterChip(selected = isMuted, onClick = { onMute(!isMuted) }, label = {
      Text(stringResource(if (isMuted) R.string.call_unmute else R.string.call_mute))
    })
    FilterChip(selected = isDeafened, onClick = { onDeafen(!isDeafened) }, label = {
      Text(stringResource(if (isDeafened) R.string.call_undeafen else R.string.call_deafen))
    })
    FilterChip(selected = isCameraOn, onClick = { onCamera(!isCameraOn) }, label = {
      Text(stringResource(if (isCameraOn) R.string.call_camera_off else R.string.call_camera_on))
    })
    if (isCameraOn) {
      FilterChip(selected = false, onClick = onFlipCamera, label = { Text(stringResource(R.string.call_camera_flip)) })
    }
    FilterChip(selected = isSpeakerOn, onClick = { onSpeaker(!isSpeakerOn) }, label = {
      Text(stringResource(R.string.call_speaker))
    })
  }
  Button(
    onClick = onHangUp,
    colors = ButtonDefaults.buttonColors(containerColor = MaterialTheme.colorScheme.error),
  ) {
    Text(stringResource(R.string.call_hang_up))
  }
}

private fun phaseText(maybeState: CallStateInfo?): Int = when (maybeState?.phase) {
  null, CallPhase.CONNECTING -> R.string.call_connecting
  CallPhase.CONNECTED -> if (maybeState.isSecured) R.string.call_secured else R.string.call_securing
  CallPhase.ENDED -> when (maybeState.maybeEndReason) {
    CallEndReason.REMOVED_BY_HOST -> R.string.call_removed_by_host
    CallEndReason.ENDED_BY_HOST -> R.string.call_ended_by_host
    CallEndReason.FAILED -> R.string.call_failed
    else -> R.string.call_ended
  }
}
