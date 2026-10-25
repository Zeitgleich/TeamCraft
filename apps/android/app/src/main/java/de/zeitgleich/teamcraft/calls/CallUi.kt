package de.zeitgleich.teamcraft.calls

import android.graphics.Bitmap
import android.provider.Settings
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.EnterTransition
import androidx.compose.animation.ExitTransition
import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.Spring
import androidx.compose.animation.core.VectorConverter
import androidx.compose.animation.core.spring
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.slideInVertically
import androidx.compose.animation.slideOutVertically
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectDragGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Lock
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.RectangleShape
import androidx.compose.ui.graphics.Shape
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import de.zeitgleich.teamcraft.R
import kotlin.math.roundToInt
import kotlinx.coroutines.launch
import uniffi.teamcraft_ffi.CallEndReason
import uniffi.teamcraft_ffi.CallPhase
import uniffi.teamcraft_ffi.CallStateInfo
import uniffi.teamcraft_ffi.VideoTrackInfo

private val STAGE_COLOR = Color(0xFF0B0D10)
private val CONTROL_COLOR = Color.White.copy(alpha = 0.14f)
private val CONTROL_SIZE = 56.dp
private val SELF_VIEW_WIDTH = 108.dp
private val SELF_VIEW_HEIGHT = 144.dp
private val SELF_VIEW_MARGIN = 12.dp
private val FILMSTRIP_TILE_WIDTH = 104.dp
private val FILMSTRIP_TILE_HEIGHT = 78.dp
private val TILE_SHAPE = RoundedCornerShape(20.dp)
private const val SECONDS_PER_MINUTE = 60L
private const val SECONDS_PER_HOUR = 3600L

/** What the call screen shows and does; the screen itself and the store screenshots build one. */
internal class CallUiState(
  val name: String,
  val maybeState: CallStateInfo?,
  val tracks: List<VideoTrackInfo>,
  val frames: Map<UInt, Bitmap>,
  val isCameraOn: Boolean,
  val isSpeakerOn: Boolean,
  val elapsedSeconds: Long?,
  val areControlsShown: Boolean,
)

internal class CallUiActions(
  val onToggleControls: () -> Unit,
  val onInteraction: () -> Unit,
  val onMute: (Boolean) -> Unit,
  val onDeafen: (Boolean) -> Unit,
  val onCamera: (Boolean) -> Unit,
  val onFlipCamera: () -> Unit,
  val onSpeaker: (Boolean) -> Unit,
  val maybeOnAddPeople: (() -> Unit)?,
  val onHangUp: () -> Unit,
)

/**
 * The in-call screen: an edge-to-edge dark stage with the others' video (one full-bleed, several
 * as a spotlight with a filmstrip) or a large avatar, the name, time and encryption chip on top,
 * round controls at the bottom, and the self-view as a draggable corner picture. A tap on the
 * stage hides or shows the chrome.
 */
@Composable
internal fun InCallContent(
  ui: CallUiState,
  actions: CallUiActions,
  modifier: Modifier = Modifier,
  banner: @Composable () -> Unit = {},
  selfView: (@Composable (Modifier) -> Unit)? = null,
) {
  val reduceMotion = rememberReducedMotion()
  Box(modifier.fillMaxSize().background(STAGE_COLOR)) {
    Stage(ui, actions.onToggleControls, Modifier.fillMaxSize())
    Column(Modifier.fillMaxSize().safeDrawingPadding()) {
      AnimatedVisibility(
        visible = ui.areControlsShown,
        enter = if (reduceMotion) EnterTransition.None else fadeIn() + slideInVertically { -it / 2 },
        exit = if (reduceMotion) ExitTransition.None else fadeOut() + slideOutVertically { -it / 2 },
      ) {
        TopBar(ui)
      }
      Column(Modifier.weight(1f).padding(horizontal = 16.dp), verticalArrangement = Arrangement.Bottom) {
        StatusHint(ui.maybeState)
        banner()
      }
      AnimatedVisibility(
        visible = ui.areControlsShown,
        enter = if (reduceMotion) EnterTransition.None else fadeIn() + slideInVertically { it / 2 },
        exit = if (reduceMotion) ExitTransition.None else fadeOut() + slideOutVertically { it / 2 },
      ) {
        ControlBar(ui, actions)
      }
    }
    if (selfView != null) SelfViewCorner(selfView, reduceMotion)
  }
}

@Composable
private fun Stage(ui: CallUiState, onToggleControls: () -> Unit, modifier: Modifier) {
  var maybeSpotlight by rememberSaveable { mutableStateOf<Long?>(null) }
  // Shared screens first: they are what people look at.
  val ordered = ui.tracks.sortedByDescending { it.isScreen }
  val spotlight = ordered.firstOrNull { it.streamId.toLong() == maybeSpotlight } ?: ordered.firstOrNull()
  val filmstrip = ordered.filter { it != spotlight }
  val toggleLabel = stringResource(R.string.call_toggle_controls)
  Box(modifier.clickable(onClickLabel = toggleLabel, onClick = onToggleControls)) {
    if (spotlight == null) {
      AvatarStage(ui.name, ui.maybeState)
    } else {
      VideoTile(spotlight, ui.frames[spotlight.streamId], Modifier.fillMaxSize(), ContentScale.Fit, RectangleShape)
      if (filmstrip.isNotEmpty()) {
        Filmstrip(filmstrip, ui.frames, onPick = { maybeSpotlight = it.streamId.toLong() })
      }
    }
  }
}

@Composable
private fun AvatarStage(name: String, maybeState: CallStateInfo?) {
  Column(
    Modifier.fillMaxSize().padding(32.dp),
    verticalArrangement = Arrangement.Center,
    horizontalAlignment = Alignment.CenterHorizontally,
  ) {
    val hue = ((name.hashCode() % 360 + 360) % 360).toFloat()
    Box(
      Modifier.size(128.dp).background(Color.hsl(hue, 0.45f, 0.45f), CircleShape),
      contentAlignment = Alignment.Center,
    ) {
      Text(
        name.firstOrNull()?.uppercase() ?: "?",
        color = Color.White,
        fontSize = 56.sp,
        fontWeight = FontWeight.Medium,
        modifier = Modifier.semantics { contentDescription = name },
      )
    }
    if (maybeState?.phase == CallPhase.CONNECTED) {
      val count = maybeState.participantCount.toInt()
      Text(
        pluralStringResource(R.plurals.call_participants, count, count),
        color = Color.White.copy(alpha = 0.7f),
        modifier = Modifier.padding(top = 20.dp),
      )
    }
  }
}

@Composable
private fun Filmstrip(tracks: List<VideoTrackInfo>, frames: Map<UInt, Bitmap>, onPick: (VideoTrackInfo) -> Unit) {
  Box(Modifier.fillMaxSize().safeDrawingPadding(), contentAlignment = Alignment.BottomCenter) {
    LazyRow(
      modifier = Modifier.fillMaxWidth().padding(bottom = FILMSTRIP_BOTTOM_PADDING),
      contentPadding = PaddingValues(horizontal = 16.dp),
      horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
      items(tracks, key = { it.streamId.toLong() }) { track ->
        val label = stringResource(R.string.call_video_spotlight)
        VideoTile(
          track,
          frames[track.streamId],
          Modifier.size(FILMSTRIP_TILE_WIDTH, FILMSTRIP_TILE_HEIGHT).clickable(onClickLabel = label) { onPick(track) },
          ContentScale.Crop,
          RoundedCornerShape(14.dp),
        )
      }
    }
  }
}

@Composable
private fun VideoTile(
  track: VideoTrackInfo,
  maybeFrame: Bitmap?,
  modifier: Modifier,
  scale: ContentScale,
  shape: Shape,
) {
  val description = stringResource(if (track.isScreen) R.string.call_video_screen else R.string.call_video_camera)
  Box(
    modifier
      .background(Color.Black, shape)
      .border(1.dp, Color.White.copy(alpha = 0.12f), shape),
    contentAlignment = Alignment.Center,
  ) {
    if (maybeFrame == null) {
      Text(stringResource(R.string.call_video_waiting), color = Color.White.copy(alpha = 0.7f), fontSize = 13.sp)
    } else {
      Image(
        bitmap = maybeFrame.asImageBitmap(),
        contentDescription = description,
        contentScale = scale,
        modifier = Modifier.fillMaxSize(),
      )
    }
  }
}

@Composable
private fun TopBar(ui: CallUiState) {
  var isSheetOpen by rememberSaveable { mutableStateOf(false) }
  val maybeState = ui.maybeState
  val isSecured = maybeState?.isSecured == true
  Column(
    Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
    horizontalAlignment = Alignment.CenterHorizontally,
    verticalArrangement = Arrangement.spacedBy(6.dp),
  ) {
    Text(
      ui.name,
      color = Color.White,
      style = MaterialTheme.typography.titleLarge,
      maxLines = 1,
      overflow = TextOverflow.Ellipsis,
    )
    Text(
      subtitle(ui),
      color = Color.White.copy(alpha = 0.7f),
      style = MaterialTheme.typography.bodyMedium,
      modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
    )
    maybeState?.maybePrivacyCode?.let {
      val chipLabel = stringResource(R.string.call_encryption_open)
      Row(
        Modifier
          .background(CONTROL_COLOR, CircleShape)
          .clickable(onClickLabel = chipLabel, role = Role.Button) { isSheetOpen = true }
          .padding(horizontal = 12.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(6.dp),
      ) {
        Icon(Icons.Filled.Lock, contentDescription = null, tint = Color.White, modifier = Modifier.size(16.dp))
        Text(
          stringResource(if (isSecured) R.string.call_secured else R.string.call_securing),
          color = Color.White,
          style = MaterialTheme.typography.labelLarge,
        )
      }
      if (isSheetOpen) EncryptionSheet(it) { isSheetOpen = false }
    }
  }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun EncryptionSheet(code: String, onDismiss: () -> Unit) {
  ModalBottomSheet(onDismissRequest = onDismiss, sheetState = rememberModalBottomSheetState()) {
    Column(
      Modifier.fillMaxWidth().padding(horizontal = 24.dp).padding(bottom = 32.dp),
      verticalArrangement = Arrangement.spacedBy(12.dp),
      horizontalAlignment = Alignment.CenterHorizontally,
    ) {
      Icon(Icons.Filled.Lock, contentDescription = null, tint = MaterialTheme.colorScheme.primary)
      Text(stringResource(R.string.call_encryption_title), style = MaterialTheme.typography.titleLarge)
      Text(
        stringResource(R.string.call_encryption_body),
        style = MaterialTheme.typography.bodyMedium,
        textAlign = TextAlign.Center,
      )
      Text(
        stringResource(R.string.call_privacy_code_hint),
        style = MaterialTheme.typography.bodySmall,
        textAlign = TextAlign.Center,
      )
      Text(code, style = MaterialTheme.typography.headlineMedium, fontFamily = FontFamily.Monospace)
    }
  }
}

/** "Connecting…", the time and the number of others, or why the call ended. */
@Composable
private fun subtitle(ui: CallUiState): String {
  val state = ui.maybeState
  val time = ui.elapsedSeconds?.let { formatDuration(it) }
  return when (state?.phase) {
    null, CallPhase.CONNECTING -> stringResource(R.string.call_connecting)
    CallPhase.RECONNECTING -> stringResource(R.string.call_reconnecting)
    CallPhase.ENDED -> stringResource(
      when (state.maybeEndReason) {
        CallEndReason.REMOVED_BY_HOST -> R.string.call_removed_by_host
        CallEndReason.ENDED_BY_HOST -> R.string.call_ended_by_host
        CallEndReason.FAILED -> R.string.call_failed
        else -> R.string.call_ended
      },
    )
    CallPhase.CONNECTED -> {
      val count = state.participantCount.toInt()
      listOfNotNull(time, pluralStringResource(R.plurals.call_participants, count, count)).joinToString(" · ")
    }
  }
}

/** The hints that matter while talking: reconnecting, and that nobody hears you. */
@Composable
private fun StatusHint(maybeState: CallStateInfo?) {
  val textRes = when {
    maybeState?.phase == CallPhase.RECONNECTING -> R.string.call_reconnecting
    maybeState?.isMuted == true && maybeState.phase == CallPhase.CONNECTED -> R.string.call_you_muted
    else -> null
  } ?: return
  Surface(
    color = Color.Black.copy(alpha = 0.55f),
    contentColor = Color.White,
    shape = CircleShape,
    modifier = Modifier.padding(bottom = 12.dp).semantics { liveRegion = LiveRegionMode.Polite },
  ) {
    Text(stringResource(textRes), Modifier.padding(horizontal = 16.dp, vertical = 8.dp), style = MaterialTheme.typography.labelLarge)
  }
}

@Composable
private fun ControlBar(ui: CallUiState, actions: CallUiActions) {
  val state = ui.maybeState
  val isMuted = state?.isMuted == true
  val isDeafened = state?.isDeafened == true
  var isMenuOpen by remember { mutableStateOf(false) }
  Row(
    Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 16.dp),
    horizontalArrangement = Arrangement.SpaceEvenly,
    verticalAlignment = Alignment.CenterVertically,
  ) {
    RoundControl(
      icon = if (isMuted) CallIcons.MicOff else CallIcons.Mic,
      label = stringResource(if (isMuted) R.string.call_unmute else R.string.call_mute),
      isActive = isMuted,
    ) {
      actions.onInteraction()
      actions.onMute(!isMuted)
    }
    RoundControl(
      icon = if (ui.isCameraOn) CallIcons.Videocam else CallIcons.VideocamOff,
      label = stringResource(if (ui.isCameraOn) R.string.call_camera_off else R.string.call_camera_on),
      isActive = !ui.isCameraOn,
    ) {
      actions.onInteraction()
      actions.onCamera(!ui.isCameraOn)
    }
    if (ui.isCameraOn) {
      RoundControl(Icons.Filled.Refresh, stringResource(R.string.call_camera_flip), isActive = false) {
        actions.onInteraction()
        actions.onFlipCamera()
      }
    }
    RoundControl(CallIcons.VolumeUp, stringResource(R.string.call_speaker), isActive = ui.isSpeakerOn) {
      actions.onInteraction()
      actions.onSpeaker(!ui.isSpeakerOn)
    }
    Box {
      RoundControl(Icons.Filled.MoreVert, stringResource(R.string.call_more), isActive = false) {
        actions.onInteraction()
        isMenuOpen = true
      }
      DropdownMenu(expanded = isMenuOpen, onDismissRequest = { isMenuOpen = false }) {
        DropdownMenuItem(
          text = { Text(stringResource(if (isDeafened) R.string.call_undeafen else R.string.call_deafen)) },
          onClick = {
            isMenuOpen = false
            actions.onDeafen(!isDeafened)
          },
        )
        actions.maybeOnAddPeople?.let { onAddPeople ->
          DropdownMenuItem(
            text = { Text(stringResource(R.string.call_add_people)) },
            leadingIcon = { Icon(Icons.Filled.Add, contentDescription = null) },
            onClick = {
              isMenuOpen = false
              onAddPeople()
            },
          )
        }
      }
    }
    RoundControl(
      icon = CallIcons.CallEnd,
      label = stringResource(R.string.call_hang_up),
      isActive = false,
      container = MaterialTheme.colorScheme.error,
      content = MaterialTheme.colorScheme.onError,
      onClick = actions.onHangUp,
    )
  }
}

/** A round icon button (at least 48 dp for the finger); a switched-on state is a filled white one. */
@Composable
private fun RoundControl(
  icon: ImageVector,
  label: String,
  isActive: Boolean,
  container: Color = if (isActive) Color.White else CONTROL_COLOR,
  content: Color = if (isActive) STAGE_COLOR else Color.White,
  onClick: () -> Unit,
) {
  Box(
    Modifier
      .size(CONTROL_SIZE)
      .background(container, CircleShape)
      .clickable(role = Role.Button, onClick = onClick)
      .semantics { contentDescription = label },
    contentAlignment = Alignment.Center,
  ) {
    Icon(icon, contentDescription = null, tint = content, modifier = Modifier.size(26.dp))
  }
}

/** The self-view in a corner: drag it and it springs to the nearest one. */
@Composable
private fun SelfViewCorner(selfView: @Composable (Modifier) -> Unit, reduceMotion: Boolean) {
  val scope = rememberCoroutineScope()
  BoxWithConstraints(Modifier.fillMaxSize().safeDrawingPadding()) {
    val density = LocalDensity.current
    val area = with(density) { Offset(maxWidth.toPx(), maxHeight.toPx()) }
    val pipSize = with(density) { Offset(SELF_VIEW_WIDTH.toPx(), SELF_VIEW_HEIGHT.toPx()) }
    val margin = with(density) { SELF_VIEW_MARGIN.toPx() }
    // Below the top bar, above the controls.
    val top = with(density) { TOP_CLEARANCE.toPx() }
    val bottom = with(density) { BOTTOM_CLEARANCE.toPx() }
    val corners = listOf(
      Offset(margin, top),
      Offset(area.x - pipSize.x - margin, top),
      Offset(margin, area.y - pipSize.y - bottom),
      Offset(area.x - pipSize.x - margin, area.y - pipSize.y - bottom),
    )
    val position = remember { Animatable(Offset.Zero, Offset.VectorConverter) }
    LaunchedEffect(area) { position.snapTo(corners[1]) }
    selfView(
      Modifier
        .offset { IntOffset(position.value.x.roundToInt(), position.value.y.roundToInt()) }
        .size(SELF_VIEW_WIDTH, SELF_VIEW_HEIGHT)
        .clip(TILE_SHAPE)
        .background(Color.Black)
        .pointerInput(area) {
          detectDragGestures(
            onDrag = { change, drag ->
              change.consume()
              scope.launch { position.snapTo(position.value + drag) }
            },
            onDragEnd = {
              val centre = position.value + Offset(pipSize.x / 2, pipSize.y / 2)
              val target = corners.minBy { (it + Offset(pipSize.x / 2, pipSize.y / 2) - centre).getDistance() }
              scope.launch {
                if (reduceMotion) {
                  position.snapTo(target)
                } else {
                  position.animateTo(target, spring(dampingRatio = Spring.DampingRatioMediumBouncy, stiffness = Spring.StiffnessMedium))
                }
              }
            },
          )
        },
    )
  }
}

/** Animations are off when the user turned them off (accessibility or developer settings). */
@Composable
private fun rememberReducedMotion(): Boolean {
  val resolver = LocalContext.current.contentResolver
  return remember(resolver) {
    Settings.Global.getFloat(resolver, Settings.Global.ANIMATOR_DURATION_SCALE, 1f) == 0f
  }
}

internal fun formatDuration(seconds: Long): String {
  val hours = seconds / SECONDS_PER_HOUR
  val minutes = seconds % SECONDS_PER_HOUR / SECONDS_PER_MINUTE
  val rest = seconds % SECONDS_PER_MINUTE
  return if (hours > 0) "%d:%02d:%02d".format(hours, minutes, rest) else "%d:%02d".format(minutes, rest)
}

private val FILMSTRIP_BOTTOM_PADDING = 104.dp
private val TOP_CLEARANCE = 140.dp
private val BOTTOM_CLEARANCE = 104.dp
