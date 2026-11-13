package de.zeitgleich.teamcraft.calls

import android.os.Bundle
import android.util.Log
import android.view.WindowManager.LayoutParams.FLAG_SECURE
import android.widget.Toast
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.size
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.TeamCraftApplication
import de.zeitgleich.teamcraft.core.ChatClient
import de.zeitgleich.teamcraft.core.CoreRepository
import de.zeitgleich.teamcraft.core.User
import de.zeitgleich.teamcraft.security.AppLockPrefs
import de.zeitgleich.teamcraft.security.ScreenPrefs.Companion.prefs
import de.zeitgleich.teamcraft.ui.BubbleActivity
import de.zeitgleich.teamcraft.ui.theme.TeamCraftTheme
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.serialization.SerializationException
import uniffi.teamcraft_ffi.MeetingHostAction
import uniffi.teamcraft_ffi.TeamCraftException

/**
 * "Hand over" on the reminder after the organizer left a meeting's call from the system's call UI
 * with people still in the lobby (#1859): whom to hand the meeting to, read afresh from the core
 * (the members still in the call, no guests), as the in-call leave question offers them. With the
 * app lock on, the app opens on the meeting chat instead: names stay behind the lock.
 */
class HandoverActivity : ComponentActivity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    val conversationId = intent?.getStringExtra(EXTRA_CONVERSATION_ID)
    if (conversationId == null) {
      finish()
      return
    }
    if (AppLockPrefs.prefs(this).isEnabled) {
      BubbleActivity.openInApp(this, conversationId)
      finish()
      return
    }
    if (prefs(this).isSecureScreen) window.addFlags(FLAG_SECURE)
    val core = TeamCraftApplication.graph(this).core
    setContent {
      TeamCraftTheme {
        HandoverChoice(core, conversationId, onDone = { maybeName ->
          TeamCraftApplication.graph(this).notifier.removeLobbyLeftWaiting(conversationId)
          maybeName?.let { Toast.makeText(this, getString(R.string.lobby_left_handed_over, it), Toast.LENGTH_LONG).show() }
          finish()
        })
      }
    }
  }

  companion object {
    const val EXTRA_CONVERSATION_ID = "de.zeitgleich.teamcraft.calls.extra.HANDOVER_CONVERSATION_ID"
  }
}

/** What the handover choice shows. */
private sealed interface HandoverStep {
  data object Loading : HandoverStep

  /** Nobody waits any more, or nobody is left to take the meeting. */
  data object Nothing : HandoverStep

  /** [meetingTitle]: which meeting it is, shown above the choice (blank if it has none). */
  data class Choose(val meetingTitle: String, val waiting: Int, val candidates: List<User>) : HandoverStep

  /** The core couldn't read it (offline): said, not shown as "nothing to do". */
  data object Unreadable : HandoverStep

  data class Saving(val to: User) : HandoverStep

  data class Failed(val to: User, val maybeReason: String?) : HandoverStep
}

/** [onDone]: the name of whom it went to, or `null` when closed without a handover. */
@Composable
private fun HandoverChoice(core: CoreRepository, conversationId: String, onDone: (String?) -> Unit) {
  var step by remember { mutableStateOf<HandoverStep>(HandoverStep.Loading) }
  // Kept through saving and a failure, so the dialog always says which meeting it hands over.
  var meetingTitle by remember { mutableStateOf("") }
  val scope = rememberCoroutineScope()
  LaunchedEffect(conversationId) {
    val loaded = loadChoice(core, conversationId)
    if (loaded is HandoverStep.Choose) meetingTitle = loaded.meetingTitle
    step = loaded
  }
  val handOver = { user: User ->
    step = HandoverStep.Saving(user)
    scope.launch {
      step = when (val result = handOver(core, conversationId, user)) {
        HostsChange.Idle -> {
          onDone(user.displayName)
          return@launch
        }
        is HostsChange.Failed -> HandoverStep.Failed(user, result.maybeReason)
        HostsChange.Saving -> HandoverStep.Saving(user)
      }
    }
  }
  AlertDialog(
    onDismissRequest = { onDone(null) },
    title = { Text(stringResource(R.string.call_hosts_leave_title)) },
    text = {
      Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        if (meetingTitle.isNotBlank()) {
          Text(meetingTitle, style = MaterialTheme.typography.titleMedium, maxLines = 2, overflow = TextOverflow.Ellipsis)
        }
        when (val shown = step) {
          HandoverStep.Loading -> Busy(stringResource(R.string.lobby_left_checking))
          HandoverStep.Nothing -> Text(stringResource(R.string.lobby_left_nothing))
          HandoverStep.Unreadable -> Text(stringResource(R.string.error_network), color = MaterialTheme.colorScheme.error)
          is HandoverStep.Choose -> {
            Text(pluralStringResource(R.plurals.call_hosts_leave_body, shown.waiting, shown.waiting))
            shown.candidates.forEach { user ->
              Button(onClick = { handOver(user) }, modifier = Modifier.fillMaxWidth()) {
                Text(stringResource(R.string.lobby_left_hand_over_to, user.displayName), maxLines = 1, overflow = TextOverflow.Ellipsis)
              }
            }
          }
          is HandoverStep.Saving -> Busy(stringResource(R.string.call_hosts_saving))
          is HandoverStep.Failed -> {
            Text(
              stringResource(R.string.call_hosts_failed, shown.maybeReason ?: stringResource(R.string.error_generic)),
              color = MaterialTheme.colorScheme.error,
              modifier = Modifier.semantics { liveRegion = LiveRegionMode.Assertive },
            )
            Button(onClick = { handOver(shown.to) }, modifier = Modifier.fillMaxWidth()) {
              Text(stringResource(R.string.lobby_left_hand_over_to, shown.to.displayName), maxLines = 1, overflow = TextOverflow.Ellipsis)
            }
          }
        }
      }
    },
    confirmButton = { TextButton(onClick = { onDone(null) }) { Text(stringResource(R.string.close)) } },
  )
}

@Composable
private fun Busy(text: String) {
  Row(
    Modifier.semantics { liveRegion = LiveRegionMode.Polite },
    horizontalArrangement = Arrangement.spacedBy(8.dp),
    verticalAlignment = Alignment.CenterVertically,
  ) {
    CircularProgressIndicator(Modifier.size(16.dp), strokeWidth = 2.dp)
    Text(text, style = MaterialTheme.typography.bodyMedium)
  }
}

private const val TAG = "TeamCraftCalls"

/** Who could take the meeting now, read afresh; [HandoverStep.Nothing] when nothing is left to do. */
private suspend fun loadChoice(core: CoreRepository, conversationId: String): HandoverStep =
  try {
    withContext(Dispatchers.Default) {
      val left = core.core().lobbyLeftWaiting(conversationId) ?: return@withContext HandoverStep.Nothing
      val snapshot = ChatClient { core.core() }.snapshot()
      val candidates = left.candidateUserIds.mapNotNull { snapshot.user(it) }.take(MAX_HANDOVER_CHOICES)
      if (candidates.isEmpty()) HandoverStep.Nothing else HandoverStep.Choose(left.meetingTitle, left.waiting.toInt(), candidates)
    }
  } catch (error: TeamCraftException) {
    Log.w(TAG, "could not read whom to hand the meeting to", error)
    HandoverStep.Unreadable
  } catch (error: SerializationException) {
    HandoverStep.Nothing
  } catch (error: IllegalStateException) {
    if (error is CancellationException) throw error
    HandoverStep.Nothing
  }

/** The core signs the handover; this waits until it is saved or refused. */
private suspend fun handOver(core: CoreRepository, conversationId: String, user: User): HostsChange =
  try {
    core.core().changeMeetingHost(conversationId, user.id, MeetingHostAction.HAND_OVER)
    awaitHostsChange(core, conversationId)
  } catch (error: TeamCraftException) {
    Log.w(TAG, "the meeting could not be handed over", error)
    HostsChange.Failed(null)
  } catch (error: IllegalStateException) {
    if (error is CancellationException) throw error
    HostsChange.Failed(null)
  }
