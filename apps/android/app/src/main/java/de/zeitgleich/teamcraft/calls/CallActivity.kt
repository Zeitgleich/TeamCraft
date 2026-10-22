package de.zeitgleich.teamcraft.calls

import android.Manifest
import android.content.pm.PackageManager
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.TeamCraftApplication
import de.zeitgleich.teamcraft.ui.theme.TeamCraftTheme
import uniffi.teamcraft_ffi.CallEndReason
import uniffi.teamcraft_ffi.CallPhase
import uniffi.teamcraft_ffi.CallStateInfo

/** The in-call screen: state, the privacy code to compare, mute and hang up. */
class CallActivity : ComponentActivity() {
  private val calls by lazy { TeamCraftApplication.graph(this).calls }

  private val microphonePermission = registerForActivityResult(ActivityResultContracts.RequestPermission()) {
    calls.onMicrophonePermission(it)
  }

  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    enableEdgeToEdge()
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
        CallScreen(
          maybeState = maybeState,
          onMute = { calls.setMuted(it) },
          onHangUp = { calls.hangUp() },
        )
      }
    }
  }
}

@Composable
private fun CallScreen(maybeState: CallStateInfo?, onMute: (Boolean) -> Unit, onHangUp: () -> Unit) {
  Surface(modifier = Modifier.fillMaxSize()) {
    Column(
      modifier = Modifier.padding(24.dp),
      verticalArrangement = Arrangement.spacedBy(16.dp, Alignment.CenterVertically),
      horizontalAlignment = Alignment.CenterHorizontally,
    ) {
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
      Row(horizontalArrangement = Arrangement.spacedBy(16.dp)) {
        val isMuted = maybeState?.isMuted == true
        OutlinedButton(onClick = { onMute(!isMuted) }) {
          Text(stringResource(if (isMuted) R.string.call_unmute else R.string.call_mute))
        }
        Button(
          onClick = onHangUp,
          colors = ButtonDefaults.buttonColors(containerColor = MaterialTheme.colorScheme.error),
        ) {
          Text(stringResource(R.string.call_hang_up))
        }
      }
    }
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
