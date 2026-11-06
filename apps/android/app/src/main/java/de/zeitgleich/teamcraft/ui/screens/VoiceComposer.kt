package de.zeitgleich.teamcraft.ui.screens

import android.Manifest
import android.app.Activity
import android.content.Context
import android.content.ContextWrapper
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.provider.Settings
import android.widget.Toast
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.waitForUpOrCancellation
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.Send
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.FilledIconButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalLayoutDirection
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.onClick
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.LayoutDirection
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LifecycleEventEffect
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.PickedFile
import de.zeitgleich.teamcraft.core.VoiceMessagePolicy
import de.zeitgleich.teamcraft.ui.HoldGesture
import de.zeitgleich.teamcraft.ui.MainViewModel
import de.zeitgleich.teamcraft.ui.VoiceGestures
import de.zeitgleich.teamcraft.ui.VoiceOwner
import de.zeitgleich.teamcraft.ui.VoicePrefs
import de.zeitgleich.teamcraft.ui.VoiceRefusal
import de.zeitgleich.teamcraft.ui.VoiceState
import de.zeitgleich.teamcraft.ui.voiceStopText
import kotlinx.coroutines.launch
import uniffi.teamcraft_ffi.VoiceStop
import uniffi.teamcraft_ffi.voiceWaveformHeights

/** Bars of a take's waveform in the review. */
private const val REVIEW_BARS = 40u

/**
 * The composer's voice message controls (#204): the state of the app's one recording as far as it
 * belongs to this composer, the bar that replaces the text field while recording or reviewing, and
 * the round button (record, or send a take).
 *
 * - Hold the button to record, let go to send; slide towards the start to cancel, up to go on
 *   hands-free. A tap records hands-free: Discard, Stop (to listen to the length and waveform
 *   first) and Send.
 * - The microphone permission is asked at first use, after a word on why; a permanent refusal says
 *   where to allow it.
 * - Leaving the conversation, the app going to the background and a call stop the recording; the
 *   take then waits here. Nothing is sent without the user's Send (or letting go of a hold).
 */
class VoiceControls(
  val state: VoiceState,
  val owner: VoiceOwner,
) {
  /** This composer shows a recording or a take instead of its text field. */
  val isActive: Boolean get() = state.maybeOwner == owner
}

@Composable
internal fun rememberVoiceControls(viewModel: MainViewModel, owner: VoiceOwner): VoiceControls {
  val state by viewModel.voice.current.collectAsState()
  val context = LocalContext.current
  val currentOwner by rememberUpdatedState(owner)
  // The app going to the background (or the screen locking) stops the recording; the take waits.
  LifecycleEventEffect(Lifecycle.Event.ON_STOP) { viewModel.voice.interrupt(VoiceStop.BACKGROUND, currentOwner) }
  DisposableEffect(owner) {
    onDispose {
      // Leaving the conversation stops its recording; a rotation only redraws the composer.
      if (context.findActivity()?.isChangingConfigurations != true) viewModel.voice.interrupt(VoiceStop.LEFT, owner)
    }
  }
  return VoiceControls(state, owner)
}

/** The bar in place of the text field: the running recording, or the take to send. */
@Composable
internal fun VoiceBar(viewModel: MainViewModel, controls: VoiceControls, modifier: Modifier = Modifier) {
  val context = LocalContext.current
  Row(modifier.height(48.dp).padding(horizontal = 4.dp), verticalAlignment = Alignment.CenterVertically) {
    when (val state = controls.state) {
      is VoiceState.Starting -> {
        CircularProgressIndicator(Modifier.padding(horizontal = 12.dp).size(20.dp), strokeWidth = 2.dp)
        Text(stringResource(R.string.voice_starting), style = MaterialTheme.typography.bodyMedium)
      }
      is VoiceState.Recording -> {
        if (state.isLocked) DiscardButton { viewModel.voice.discard() }
        RecordingDot()
        Text(
          formatVideoDuration(state.elapsedMs),
          style = MaterialTheme.typography.bodyMedium,
          modifier = Modifier.padding(horizontal = 8.dp).semantics { liveRegion = LiveRegionMode.Polite },
        )
        if (state.isLocked) {
          LiveBars(state.levels, Modifier.weight(1f))
          IconButton(onClick = { viewModel.voice.stop() }) {
            Icon(painterResource(R.drawable.ic_stop), contentDescription = stringResource(R.string.voice_stop))
          }
        } else {
          Text(
            stringResource(R.string.voice_hold_hint),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            maxLines = 2,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.weight(1f),
          )
          Icon(painterResource(R.drawable.ic_lock), contentDescription = null, tint = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.size(18.dp))
        }
      }
      is VoiceState.Finishing -> {
        CircularProgressIndicator(Modifier.padding(horizontal = 12.dp).size(20.dp), strokeWidth = 2.dp)
        Text(stringResource(R.string.voice_finishing), style = MaterialTheme.typography.bodyMedium)
      }
      is VoiceState.Review -> {
        DiscardButton { viewModel.voice.discard() }
        val heights = remember(state.take.path) { voiceWaveformHeights(state.take.waveform, REVIEW_BARS) }
        val reason = voiceStopText(context, state.take.maybeStop)
        Waveform(
          heights = heights,
          playedFraction = 0f,
          color = MaterialTheme.colorScheme.onSurfaceVariant,
          playedColor = MaterialTheme.colorScheme.primary,
          modifier = Modifier.weight(1f).height(28.dp).semantics {
            contentDescription = listOfNotNull(context.getString(R.string.voice_take, formatVideoDuration(state.take.durationMs.toLong())), reason).joinToString(". ")
          },
        )
        Text(formatVideoDuration(state.take.durationMs.toLong()), style = MaterialTheme.typography.bodyMedium, modifier = Modifier.padding(horizontal = 8.dp))
      }
      VoiceState.Idle -> {}
    }
  }
  val maybeReview = controls.state as? VoiceState.Review
  voiceStopText(context, maybeReview?.take?.maybeStop)?.let { reason ->
    Text(
      reason,
      style = MaterialTheme.typography.labelSmall,
      color = MaterialTheme.colorScheme.onSurfaceVariant,
      modifier = Modifier.padding(horizontal = 16.dp).semantics { liveRegion = LiveRegionMode.Polite },
    )
  }
}

/**
 * The round button beside the field when there's no text: hold to record, tap for hands-free. While
 * this composer records hands-free or has a take, it sends.
 */
@Composable
internal fun VoiceButton(
  viewModel: MainViewModel,
  controls: VoiceControls,
  policy: VoiceMessagePolicy,
  size: Dp,
  onSend: (PickedFile) -> Unit,
  modifier: Modifier = Modifier,
) {
  val context = LocalContext.current
  val scope = rememberCoroutineScope()
  val isInCall by viewModel.isInCall.collectAsState()
  val refusal by viewModel.voice.refusal.collectAsState()
  var isExplaining by remember { mutableStateOf(false) }
  var isBlocked by remember { mutableStateOf(false) }
  // Letting go of a hold sends the take once it is finished, unless something stopped it first.
  var sendsWhenFinished by remember { mutableStateOf(false) }
  val currentOnSend by rememberUpdatedState(onSend)
  val prefs = remember { VoicePrefs.prefs(context) }
  val permission = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { isGranted ->
    prefs.hasAskedMicrophone = true
    if (!isGranted) isBlocked = context.findActivity()?.shouldShowRequestPermissionRationale(Manifest.permission.RECORD_AUDIO) == false
  }
  fun hasMicrophone() = ContextCompat.checkSelfPermission(context, Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED

  /** Whether recording may start now; otherwise asks (after a word on why) or says where to allow it. */
  fun mayRecord(): Boolean {
    if (hasMicrophone()) return true
    val activity = context.findActivity()
    val isRefusedForGood = prefs.hasAskedMicrophone && activity?.shouldShowRequestPermissionRationale(Manifest.permission.RECORD_AUDIO) == false
    if (isRefusedForGood) isBlocked = true else isExplaining = true
    return false
  }

  fun sendTake() = scope.launch {
    val maybeFile = viewModel.voice.takeForSending()
    if (maybeFile != null) currentOnSend(maybeFile) else Toast.makeText(context, R.string.voice_send_failed, Toast.LENGTH_LONG).show()
  }
  val state = controls.state
  LaunchedEffect(state) {
    val review = state as? VoiceState.Review ?: return@LaunchedEffect
    if (sendsWhenFinished && review.owner == controls.owner) {
      sendsWhenFinished = false
      // Stopped by a call or the background before the finger let go: the user decides.
      if (review.take.maybeStop == null) sendTake()
    }
  }
  val isMine = controls.isActive
  val sends = isMine && (state is VoiceState.Review || (state is VoiceState.Recording && state.isLocked))
  val isBusyElsewhere = state != VoiceState.Idle && !isMine
  val density = LocalDensity.current
  val isRtl = LocalLayoutDirection.current == LayoutDirection.Rtl
  val recordLabel = stringResource(R.string.voice_record)
  if (sends) {
    FilledIconButton(
      onClick = {
        if (state is VoiceState.Recording) {
          // Hands-free: Send finishes the take and sends it.
          sendsWhenFinished = true
          viewModel.voice.stop()
        } else {
          sendTake()
        }
      },
      shape = CircleShape,
      modifier = modifier.size(size),
    ) {
      Icon(Icons.AutoMirrored.Filled.Send, contentDescription = stringResource(R.string.voice_send))
    }
  } else {
    val isHolding = isMine && (state is VoiceState.Recording || state is VoiceState.Starting)
    Box(
      modifier
        .size(size)
        .alpha(if (isBusyElsewhere || isInCall) 0.5f else 1f)
        .background(if (isHolding) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.primary, CircleShape)
        .semantics {
          contentDescription = recordLabel
          onClick(label = recordLabel) {
            if (!isBusyElsewhere && mayRecord()) viewModel.voice.start(controls.owner, policy.maxSeconds, isLocked = true, isInCall = isInCall)
            true
          }
        }
        .pointerInput(controls.owner, policy, isBusyElsewhere, isInCall) {
          if (isBusyElsewhere) return@pointerInput
          awaitEachGesture {
            val down = awaitFirstDown()
            val maybeUp = withTimeoutOrNull(VoiceGestures.HOLD_DELAY_MS) { waitForUpOrCancellation() }
            if (maybeUp != null) {
              // A tap: hands-free.
              if (mayRecord()) viewModel.voice.start(controls.owner, policy.maxSeconds, isLocked = true, isInCall = isInCall)
              return@awaitEachGesture
            }
            if (!mayRecord()) return@awaitEachGesture
            viewModel.voice.start(controls.owner, policy.maxSeconds, isLocked = false, isInCall = isInCall)
            var gesture = HoldGesture.RECORDING
            while (true) {
              val event = awaitPointerEvent()
              val change = event.changes.firstOrNull { it.id == down.id } ?: break
              val moved = change.position - down.position
              val dx = with(density) { moved.x.toDp().value } * if (isRtl) -1 else 1
              val dy = with(density) { moved.y.toDp().value }
              gesture = VoiceGestures.of(dx, dy)
              if (gesture == HoldGesture.LOCK) {
                viewModel.voice.lock()
                return@awaitEachGesture
              }
              if (!change.pressed) break
              change.consume()
            }
            when (gesture) {
              HoldGesture.CANCEL -> viewModel.voice.discard()
              else -> {
                sendsWhenFinished = true
                viewModel.voice.stop()
              }
            }
          }
        },
      contentAlignment = Alignment.Center,
    ) {
      Icon(painterResource(R.drawable.ic_mic), contentDescription = null, tint = MaterialTheme.colorScheme.onPrimary)
    }
  }
  if (isExplaining) {
    AlertDialog(
      onDismissRequest = { isExplaining = false },
      title = { Text(stringResource(R.string.voice_permission_title)) },
      text = { Text(stringResource(R.string.voice_permission_text)) },
      confirmButton = {
        TextButton(onClick = {
          isExplaining = false
          permission.launch(Manifest.permission.RECORD_AUDIO)
        }) { Text(stringResource(R.string.voice_permission_continue)) }
      },
      dismissButton = { TextButton(onClick = { isExplaining = false }) { Text(stringResource(R.string.cancel)) } },
    )
  }
  if (isBlocked) {
    AlertDialog(
      onDismissRequest = { isBlocked = false },
      title = { Text(stringResource(R.string.voice_permission_title)) },
      text = { Text(stringResource(R.string.voice_permission_blocked)) },
      confirmButton = {
        TextButton(onClick = {
          isBlocked = false
          context.startActivity(Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS, Uri.fromParts("package", context.packageName, null)))
        }) { Text(stringResource(R.string.voice_open_settings)) }
      },
      dismissButton = { TextButton(onClick = { isBlocked = false }) { Text(stringResource(R.string.cancel)) } },
    )
  }
  refusal?.let { refused ->
    LaunchedEffect(refused) {
      val text = when (refused) {
        VoiceRefusal.MICROPHONE -> R.string.voice_refused_microphone
        VoiceRefusal.BUSY -> R.string.voice_refused_busy
        VoiceRefusal.FAILED -> R.string.voice_refused_failed
      }
      Toast.makeText(context, text, Toast.LENGTH_LONG).show()
      viewModel.voice.refusalShown()
    }
  }
}

@Composable
private fun DiscardButton(onDiscard: () -> Unit) {
  IconButton(onClick = onDiscard) {
    Icon(painterResource(R.drawable.ic_delete), contentDescription = stringResource(R.string.voice_discard), tint = MaterialTheme.colorScheme.error)
  }
}

/** The red dot of a running recording, breathing slowly. */
@Composable
private fun RecordingDot() {
  val pulse = rememberInfiniteTransition(label = "recording")
  val alpha by pulse.animateFloat(1f, 0.35f, infiniteRepeatable(tween(800), RepeatMode.Reverse), label = "dot")
  Box(Modifier.padding(start = 8.dp).size(10.dp).alpha(alpha).background(MaterialTheme.colorScheme.error, CircleShape))
}

/** The last levels as bars, newest on the end side. */
@Composable
private fun LiveBars(levels: List<Float>, modifier: Modifier) {
  val color = MaterialTheme.colorScheme.primary
  Waveform(levels.map { it.coerceIn(0.08f, 1f) }, playedFraction = 1f, color = color, playedColor = color, modifier = modifier.height(28.dp))
}

/** Bars of [heights] (0..1); those before [playedFraction] in [playedColor]. */
@Composable
internal fun Waveform(heights: List<Float>, playedFraction: Float, color: Color, playedColor: Color, modifier: Modifier = Modifier) {
  val isRtl = LocalLayoutDirection.current == LayoutDirection.Rtl
  Canvas(modifier.fillMaxWidth()) {
    if (heights.isEmpty()) return@Canvas
    val slot = size.width / heights.size
    val barWidth = (slot * 0.6f).coerceAtLeast(1f)
    heights.forEachIndexed { index, height ->
      val barHeight = (height.coerceIn(0.08f, 1f)) * size.height
      val position = index * slot + (slot - barWidth) / 2
      val x = if (isRtl) size.width - position - barWidth else position
      val isPlayed = (index + 0.5f) / heights.size <= playedFraction
      drawRoundRect(
        color = if (isPlayed) playedColor else color,
        topLeft = Offset(x, (size.height - barHeight) / 2),
        size = Size(barWidth, barHeight),
        cornerRadius = CornerRadius(barWidth / 2),
      )
    }
  }
}

internal tailrec fun Context.findActivity(): Activity? = when (this) {
  is Activity -> this
  is ContextWrapper -> baseContext.findActivity()
  else -> null
}
