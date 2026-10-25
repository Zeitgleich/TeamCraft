package de.zeitgleich.teamcraft.calls

import android.Manifest
import android.app.Activity
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import android.os.SystemClock
import android.view.WindowManager
import android.view.WindowManager.LayoutParams.FLAG_SECURE
import androidx.activity.ComponentActivity
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.selection.toggleable
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Checkbox
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.TeamCraftApplication
import de.zeitgleich.teamcraft.security.ScreenPrefs.Companion.prefs
import de.zeitgleich.teamcraft.ui.theme.TeamCraftTheme
import de.zeitgleich.teamcraft.ui.toggled
import kotlinx.coroutines.delay
import uniffi.teamcraft_ffi.CallPhase
import uniffi.teamcraft_ffi.CallStateInfo
import uniffi.teamcraft_ffi.newRequestKey

private const val MILLIS_PER_SECOND = 1000L
private const val CONTROLS_HIDE_DELAY_MILLIS = 5000L
private val ADD_PEOPLE_LIST_HEIGHT = 320.dp

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
    // A ring only shows the incoming-call screen; the microphone is asked for when answering.
    if (intent?.action != ACTION_SHOW_INCOMING) requestMicrophoneIfMissing()
    handle(intent)
    setContent {
      TeamCraftTheme {
        val maybeState by calls.callState.collectAsStateWithLifecycle()
        val maybeRing by calls.incomingRing.collectAsStateWithLifecycle()
        val isJoining by calls.isJoining.collectAsStateWithLifecycle()
        val isIdle = maybeRing == null && !isJoining && (maybeState == null || maybeState?.phase == CallPhase.ENDED)
        LaunchedEffect(isIdle) {
          // Declined, rang out, or the call ended.
          if (isIdle) finish()
        }
        val ring = maybeRing
        if (ring != null && maybeState == null && !isJoining) {
          IncomingCallScreen(
            ring = ring,
            onAnswer = {
              requestMicrophoneIfMissing()
              calls.answer(ring.conversationId)
            },
            onDecline = { calls.decline() },
          )
        } else {
          CallScreen(calls, maybeState)
        }
      }
    }
  }

  override fun onNewIntent(intent: Intent) {
    super.onNewIntent(intent)
    setIntent(intent)
    handle(intent)
  }

  /** "Answer" on the incoming-call notification opens this screen with the conversation. */
  private fun handle(intent: Intent?) {
    if (intent?.action != ACTION_ANSWER) return
    val conversationId = intent.getStringExtra(EXTRA_CONVERSATION_ID) ?: return
    // Handled once: a recreated activity must not answer again.
    intent.action = null
    requestMicrophoneIfMissing()
    calls.answer(conversationId)
  }

  private fun requestMicrophoneIfMissing() {
    val microphone = ContextCompat.checkSelfPermission(this, Manifest.permission.RECORD_AUDIO)
    if (microphone != PackageManager.PERMISSION_GRANTED) {
      microphonePermission.launch(Manifest.permission.RECORD_AUDIO)
    }
  }

  companion object {
    /** The full-screen intent of a ring: show answer and decline. */
    const val ACTION_SHOW_INCOMING = "de.zeitgleich.teamcraft.calls.SHOW_INCOMING"
    const val ACTION_ANSWER = "de.zeitgleich.teamcraft.calls.ANSWER"
    const val EXTRA_CONVERSATION_ID = "conversation_id"
  }
}

/** Who calls, with answer and decline (the ring's full-screen screen). */
@Composable
private fun IncomingCallScreen(ring: IncomingRing, onAnswer: () -> Unit, onDecline: () -> Unit) {
  Surface(modifier = Modifier.fillMaxSize()) {
    Column(
      modifier = Modifier.fillMaxSize().safeDrawingPadding().padding(32.dp),
      verticalArrangement = Arrangement.SpaceBetween,
      horizontalAlignment = Alignment.CenterHorizontally,
    ) {
      Column(horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text(stringResource(R.string.call_incoming), style = MaterialTheme.typography.titleMedium)
        Text(ring.displayName, style = MaterialTheme.typography.headlineMedium)
        ring.maybeCallerName?.takeIf { it != ring.displayName }?.let { Text(it) }
      }
      Row(horizontalArrangement = Arrangement.spacedBy(24.dp)) {
        Button(
          onClick = onDecline,
          colors = ButtonDefaults.buttonColors(containerColor = MaterialTheme.colorScheme.error),
        ) { Text(stringResource(R.string.call_decline)) }
        Button(onClick = onAnswer) { Text(stringResource(R.string.call_answer)) }
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
  var isAddingPeople by rememberSaveable { mutableStateOf(false) }
  var maybeAddChoice by remember { mutableStateOf<AddPeopleChoice?>(null) }
  val maybeConversationId = maybeState?.conversationId
  LaunchedEffect(maybeConversationId) {
    // "Add people" only in a DM or group DM call; a meeting room's call (after a move) has none.
    maybeAddChoice = maybeConversationId?.let { calls.moves.choice(it) }
  }
  val name by produceState("", maybeConversationId) {
    value = maybeConversationId?.let { calls.displayName(it) } ?: ""
  }
  // The dialog stays through a reconnect (it keeps its request key); it opens only when connected.
  val addChoice = maybeAddChoice?.takeIf { it.conversationId == maybeConversationId }
  val canAddPeople = addChoice != null && maybeState?.phase == CallPhase.CONNECTED
  if (isAddingPeople && addChoice != null) {
    AddPeopleDialog(calls.moves, addChoice) { isAddingPeople = false }
  }

  val isConnected = maybeState?.phase == CallPhase.CONNECTED
  var maybeStartedAt by remember { mutableStateOf<Long?>(null) }
  var maybeElapsed by remember { mutableStateOf<Long?>(null) }
  LaunchedEffect(isConnected) {
    // The clock starts with the first connection and runs through reconnects.
    if (isConnected && maybeStartedAt == null) maybeStartedAt = SystemClock.elapsedRealtime()
  }
  LaunchedEffect(maybeStartedAt) {
    val startedAt = maybeStartedAt ?: return@LaunchedEffect
    while (true) {
      maybeElapsed = (SystemClock.elapsedRealtime() - startedAt) / MILLIS_PER_SECOND
      delay(MILLIS_PER_SECOND)
    }
  }

  // The chrome steps aside after a while when there is video to look at; a tap brings it back.
  val hasVideo = tracks.isNotEmpty()
  var areControlsShown by rememberSaveable { mutableStateOf(true) }
  var interactions by remember { mutableIntStateOf(0) }
  LaunchedEffect(areControlsShown, interactions, hasVideo, isConnected) {
    if (!hasVideo) {
      areControlsShown = true
    } else if (areControlsShown && isConnected) {
      delay(CONTROLS_HIDE_DELAY_MILLIS)
      areControlsShown = false
    }
  }
  if (hasVideo && !areControlsShown) ImmersiveWindow()

  val selfView: (@Composable (Modifier) -> Unit)? = if (isCameraWanted && hasCamera) {
    { modifier: Modifier -> CameraFeed(calls = calls, isFrontCamera = isFrontCamera, modifier = modifier) }
  } else {
    null
  }
  InCallContent(
    ui = CallUiState(
      name = name,
      maybeState = maybeState,
      tracks = tracks,
      frames = frames,
      isCameraOn = isCameraWanted,
      isSpeakerOn = isSpeakerOn,
      elapsedSeconds = maybeElapsed,
      areControlsShown = areControlsShown,
    ),
    actions = CallUiActions(
      onToggleControls = { areControlsShown = !areControlsShown },
      onInteraction = { interactions++ },
      onMute = { calls.setMuted(it) },
      onDeafen = { calls.setDeafened(it) },
      onCamera = { isOn ->
        if (isOn && !hasCamera) cameraPermission.launch(Manifest.permission.CAMERA) else calls.setCameraWanted(isOn)
      },
      onFlipCamera = { isFrontCamera = !isFrontCamera },
      onSpeaker = { calls.setSpeakerOn(it) },
      maybeOnAddPeople = if (canAddPeople) ({ isAddingPeople = true }) else null,
      onHangUp = { calls.hangUp() },
    ),
    banner = { CallMoveBanner(calls.moves) },
    selfView = selfView,
  )
}

/** Hides the system bars while this is shown, and lets the content use the cutout's edge too. */
@Composable
private fun ImmersiveWindow() {
  val view = LocalView.current
  DisposableEffect(view) {
    val window = (view.context as? Activity)?.window ?: return@DisposableEffect onDispose {}
    val maybeCutoutMode = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) window.attributes.layoutInDisplayCutoutMode else null
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
      window.attributes = window.attributes.apply {
        layoutInDisplayCutoutMode = WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_SHORT_EDGES
      }
    }
    val controller = WindowCompat.getInsetsController(window, view)
    controller.systemBarsBehavior = WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
    controller.hide(WindowInsetsCompat.Type.systemBars())
    onDispose {
      controller.show(WindowInsetsCompat.Type.systemBars())
      if (maybeCutoutMode != null && Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
        window.attributes = window.attributes.apply { layoutInDisplayCutoutMode = maybeCutoutMode }
      }
    }
  }
}

/**
 * A move of the call another member announced (docs/meetings.md, "A call becomes a meeting"):
 * who moved it and whom they added, and joining on a tap only. Also why following a move failed.
 */
@Composable
private fun CallMoveBanner(moves: CallMoves) {
  val maybeOffer by moves.moveOffer.collectAsStateWithLifecycle()
  val maybeFailure by moves.moveFailure.collectAsStateWithLifecycle()
  val isAccepting by moves.isAccepting.collectAsStateWithLifecycle()
  val offer = maybeOffer
  val failure = maybeFailure
  if (offer == null && failure == null) return
  Surface(
    color = MaterialTheme.colorScheme.secondaryContainer,
    shape = MaterialTheme.shapes.medium,
    modifier = Modifier.fillMaxWidth(),
  ) {
    Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
      if (offer != null) {
        Text(stringResource(R.string.call_move_offer, offer.byName, offer.addedNames.joinToString(", ")))
        Button(onClick = { moves.accept() }, enabled = !isAccepting) {
          if (isAccepting) {
            CircularProgressIndicator(Modifier.size(16.dp), strokeWidth = 2.dp)
          } else {
            Text(stringResource(R.string.call_move_join))
          }
        }
      }
      if (failure != null) {
        Text(stringResource(failure), color = MaterialTheme.colorScheme.error)
        TextButton(onClick = { moves.dismissFailure() }) { Text(stringResource(R.string.close)) }
      }
    }
  }
}

/**
 * Picks people to add to a DM or group DM call. One request key per dialog, so a retry after a
 * failure or a lost answer gets the same meeting room and adds nobody twice. Stays open (with the
 * failure) until the core answered the move.
 */
@Composable
private fun AddPeopleDialog(moves: CallMoves, choice: AddPeopleChoice, onClose: () -> Unit) {
  val requestKey = rememberSaveable { newRequestKey() }
  var picked by rememberSaveable { mutableStateOf(listOf<String>()) }
  val addState by moves.addState.collectAsStateWithLifecycle()
  val isPending = addState == AddPeopleState.Pending
  val title = stringResource(R.string.call_meeting_title)
  val close = {
    moves.addShown()
    onClose()
  }
  LaunchedEffect(addState) {
    if (addState == AddPeopleState.Done) close()
  }
  AlertDialog(
    onDismissRequest = { if (!isPending) close() },
    title = { Text(stringResource(R.string.call_add_people)) },
    text = {
      Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text(stringResource(R.string.call_add_hint), style = MaterialTheme.typography.bodySmall)
        if (choice.candidates.isEmpty()) {
          Text(stringResource(R.string.call_add_nobody))
        }
        LazyColumn(Modifier.heightIn(max = ADD_PEOPLE_LIST_HEIGHT)) {
          items(choice.candidates, key = { it.id }) { user ->
            val isPicked = user.id in picked
            Row(
              Modifier
                .fillMaxWidth()
                .toggleable(value = isPicked, enabled = !isPending, role = Role.Checkbox) { picked = toggled(picked, user.id) }
                .padding(vertical = 8.dp),
              verticalAlignment = Alignment.CenterVertically,
            ) {
              Checkbox(checked = isPicked, onCheckedChange = null, modifier = Modifier.padding(end = 12.dp))
              Column(Modifier.weight(1f)) {
                Text(user.displayName)
                Text("@" + user.username, style = MaterialTheme.typography.bodySmall)
              }
            }
          }
        }
        (addState as? AddPeopleState.Failed)?.let { failed ->
          Text(stringResource(failed.text), color = MaterialTheme.colorScheme.error)
        }
      }
    },
    confirmButton = {
      Button(
        onClick = { moves.add(choice, picked.filter { id -> choice.candidates.any { it.id == id } }, title, requestKey) },
        enabled = picked.isNotEmpty() && !isPending,
      ) {
        if (isPending) {
          CircularProgressIndicator(Modifier.size(16.dp), strokeWidth = 2.dp)
        } else {
          Text(stringResource(R.string.call_add))
        }
      }
    },
    dismissButton = {
      TextButton(onClick = close, enabled = !isPending) { Text(stringResource(R.string.cancel)) }
    },
  )
}
