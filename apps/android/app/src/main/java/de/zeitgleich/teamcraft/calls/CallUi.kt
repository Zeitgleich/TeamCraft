package de.zeitgleich.teamcraft.calls

import android.graphics.Bitmap
import android.os.SystemClock
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
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Edit
import androidx.compose.material.icons.filled.Lock
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material.icons.filled.Warning
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.Offset
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
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.ui.UserAvatar
import kotlin.math.roundToInt
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import uniffi.teamcraft_ffi.CallEndReason
import uniffi.teamcraft_ffi.CallPhase
import uniffi.teamcraft_ffi.CallStateInfo
import uniffi.teamcraft_ffi.VideoTrackInfo

private val STAGE_COLOR = Color(0xFF0B0D10)
private val SPEAKING_COLOR = Color(0xFF5C9DFF)
private val CONTROL_COLOR = Color.White.copy(alpha = 0.14f)
private val CONTROL_SIZE = 56.dp
private val SELF_VIEW_WIDTH = 108.dp
private val SELF_VIEW_HEIGHT = 144.dp
private val SELF_VIEW_MARGIN = 12.dp
private val FILMSTRIP_TILE_WIDTH = 104.dp
private val FILMSTRIP_TILE_HEIGHT = 78.dp
private val TILE_SHAPE = RoundedCornerShape(20.dp)
private val GRID_GAP = 8.dp

/** Room above the grid for the top bar, and below it for the controls (#1488). */
private val GRID_TOP_ROOM = 136.dp
private val GRID_BOTTOM_ROOM = 112.dp

/** Up to this many tiles share the screen; more scroll. */
private const val GRID_FIXED_MAX = 6
private const val SCROLLING_TILE_RATIO = 0.8f
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
  /** Display names by user ID; people not in it show without a name. */
  val names: Map<String, String> = emptyMap(),
  /**
   * The notetaker is in the call (e2ee.md §8.2): the encryption chip says the audio goes to
   * OpenRouter, as on desktop and web (issue #1290).
   */
  val isNotetakerPresent: Boolean = false,
)

/** Somebody else in the call, as the tiles show them. */
private class Person(
  val userId: String,
  val name: String,
  val isSpeaking: Boolean,
  val isRejoining: Boolean,
  val isMuted: Boolean = false,
)

private fun CallUiState.people(): List<Person> {
  val state = maybeState ?: return emptyList()
  val speaking = state.speakingUserIds
  // Speakers first, loudest first; the rest by name.
  return state.participantUserIds
    .map { id -> Person(id, names[id].orEmpty(), id in speaking, id in state.reconnectingUserIds, id in state.mutedUserIds) }
    .sortedWith(compareBy({ speaking.indexOf(it.userId).let { index -> if (index < 0) Int.MAX_VALUE else index } }, { it.name.lowercase() }))
}

private fun CallUiState.ownerOf(track: VideoTrackInfo): Person? =
  maybeState?.peers?.firstOrNull { it.deviceId == track.deviceId }?.let { peer ->
    people().firstOrNull { it.userId == peer.userId }
  }

/** What a tile of the grid shows (#1488). */
private sealed interface StageTile {
  class Camera(val track: VideoTrackInfo, val maybeOwner: Person?) : StageTile

  class Avatar(val person: Person) : StageTile
}

/**
 * The grid's tiles: each person once, with their camera when it is on (speakers first, as
 * [people] orders them), then cameras nobody could be matched to. Alone or still connecting: the
 * one being called.
 */
private fun stageTiles(ui: CallUiState): List<StageTile> {
  val people = ui.people()
  val cameras = ui.tracks.filterNot { it.isScreen }
  val owners = cameras.associateWith { ui.ownerOf(it) }
  val tiles = people.map { person ->
    owners.entries.firstOrNull { it.value?.userId == person.userId }?.let { StageTile.Camera(it.key, person) } ?: StageTile.Avatar(person)
  }
  val unmatched = cameras.filter { owners[it] == null }.map { StageTile.Camera(it, null) }
  return (tiles + unmatched).ifEmpty { listOf(StageTile.Avatar(Person("", ui.name, isSpeaking = false, isRejoining = false))) }
}

/** One column for one or two people (stacked, as a phone is tall), two from three on. */
internal fun gridColumns(tileCount: Int): Int = if (tileCount <= 2) 1 else 2

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
  /** "Add notetaker" (e2ee.md §8.2): offered to whoever the policy lets ask it in. */
  val maybeOnAddNotetaker: (() -> Unit)? = null,
  /**
   * Why "Add notetaker" shows, but disabled, for whoever the policy keeps from asking it in (a
   * string resource; issue #1290).
   */
  val maybeNotetakerRefusal: Int? = null,
)

/**
 * The in-call screen: an edge-to-edge dark stage with everybody as a tile of a grid (camera or
 * avatar; a shared screen as a spotlight with a filmstrip), the name, time and encryption chip on top,
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
        EncryptionFailureBanner(ui.maybeState, actions.onHangUp)
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
    // A shared screen gets the stage; otherwise everybody is a tile of the grid (#1488).
    if (ordered.none { it.isScreen }) {
      GridStage(ui)
    } else if (spotlight != null) {
      VideoTile(spotlight, ui.frames[spotlight.streamId], ui.ownerOf(spotlight), Modifier.fillMaxSize(), ContentScale.Fit, RectangleShape)
      if (filmstrip.isNotEmpty()) {
        Filmstrip(filmstrip, ui, onPick = { maybeSpotlight = it.streamId.toLong() })
      }
    }
  }
}

/**
 * The call as a grid (#1488): a tile with rounded corners per person, their camera when it is on,
 * their avatar otherwise, and the name on the tile; whoever speaks has a ring. Up to six tiles
 * share the screen; more scroll in two columns. The top bar and the controls keep their room.
 */
@Composable
private fun GridStage(ui: CallUiState) {
  val tiles = stageTiles(ui)
  val columns = gridColumns(tiles.size)
  val padding = PaddingValues(start = 12.dp, end = 12.dp, top = GRID_TOP_ROOM, bottom = GRID_BOTTOM_ROOM)
  if (tiles.size > GRID_FIXED_MAX) {
    LazyVerticalGrid(
      columns = GridCells.Fixed(columns),
      modifier = Modifier.fillMaxSize().safeDrawingPadding(),
      contentPadding = padding,
      horizontalArrangement = Arrangement.spacedBy(GRID_GAP),
      verticalArrangement = Arrangement.spacedBy(GRID_GAP),
    ) {
      items(tiles.size) { index -> StageTileView(tiles[index], ui, Modifier.aspectRatio(SCROLLING_TILE_RATIO)) }
    }
    return
  }
  Column(
    Modifier.fillMaxSize().safeDrawingPadding().padding(padding),
    verticalArrangement = Arrangement.spacedBy(GRID_GAP),
  ) {
    for (row in tiles.chunked(columns)) {
      Row(Modifier.weight(1f).fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(GRID_GAP)) {
        for (tile in row) StageTileView(tile, ui, Modifier.weight(1f).fillMaxHeight())
        // A short last row keeps the tiles' width.
        repeat(columns - row.size) { Spacer(Modifier.weight(1f)) }
      }
    }
  }
}

/** One tile: a camera, or an avatar on a tinted card, with the name on it. */
@Composable
private fun StageTileView(tile: StageTile, ui: CallUiState, modifier: Modifier) {
  when (tile) {
    is StageTile.Camera -> VideoTile(tile.track, ui.frames[tile.track.streamId], tile.maybeOwner, modifier, ContentScale.Crop, TILE_SHAPE)
    is StageTile.Avatar -> AvatarTile(tile.person, modifier)
  }
}

@Composable
private fun AvatarTile(person: Person, modifier: Modifier) {
  val name = person.name
  val hue = ((name.hashCode() % 360 + 360) % 360).toFloat()
  BoxWithConstraints(
    modifier
      .alpha(if (person.isRejoining) 0.5f else 1f)
      .clip(TILE_SHAPE)
      .background(Color.hsl(hue, 0.25f, 0.18f))
      .border(
        if (person.isSpeaking) 3.dp else 1.dp,
        if (person.isSpeaking) SPEAKING_COLOR else Color.White.copy(alpha = 0.08f),
        TILE_SHAPE,
      ),
    contentAlignment = Alignment.Center,
  ) {
    // As large as the tile allows, within reason: a lone tile's avatar isn't a poster.
    val size = (minOf(maxWidth, maxHeight) * 0.45f).coerceIn(48.dp, 128.dp)
    UserAvatar(person.userId, Modifier.size(size)) {
      Box(Modifier.size(size).background(Color.hsl(hue, 0.45f, 0.45f), CircleShape), contentAlignment = Alignment.Center) {
        Text(
          name.firstOrNull()?.uppercase() ?: "?",
          color = Color.White,
          fontSize = (size.value * 0.42f).sp,
          fontWeight = FontWeight.Medium,
        )
      }
    }
    NameLabel(name, person.isMuted, Modifier.align(Alignment.BottomStart))
  }
}

/** The name at the tile's bottom left, on a dark pill so it reads on any picture. */
@Composable
private fun NameLabel(name: String, isMuted: Boolean, modifier: Modifier) {
  if (name.isEmpty()) return
  val mutedName = stringResource(R.string.call_person_muted, name)
  Row(
    modifier
      .padding(8.dp)
      .background(Color.Black.copy(alpha = 0.55f), CircleShape)
      .padding(horizontal = 8.dp, vertical = 2.dp)
      // Said as one: who, and that they are muted.
      .then(if (isMuted) Modifier.semantics(mergeDescendants = true) { contentDescription = mutedName } else Modifier),
    verticalAlignment = Alignment.CenterVertically,
    horizontalArrangement = Arrangement.spacedBy(4.dp),
  ) {
    if (isMuted) Icon(CallIcons.MicOff, contentDescription = null, tint = Color.White, modifier = Modifier.size(14.dp))
    Text(name, color = Color.White, style = MaterialTheme.typography.labelMedium, maxLines = 1, overflow = TextOverflow.Ellipsis)
  }
}

/** A small crossed-out microphone on a muted person's avatar. */
@Composable
private fun MutedBadge(modifier: Modifier) {
  Box(
    modifier.size(26.dp).background(Color(0xFF1B2230), CircleShape).border(1.dp, Color.White.copy(alpha = 0.25f), CircleShape),
    contentAlignment = Alignment.Center,
  ) {
    Icon(CallIcons.MicOff, contentDescription = null, tint = Color.White, modifier = Modifier.size(15.dp))
  }
}

/** The ring around whoever speaks: visible, and not only through colour (it is a thick border). */
private fun Modifier.speakingRing(isSpeaking: Boolean): Modifier =
  if (isSpeaking) padding(3.dp).border(3.dp, SPEAKING_COLOR, CircleShape).padding(3.dp) else padding(6.dp)

@Composable
private fun Filmstrip(tracks: List<VideoTrackInfo>, ui: CallUiState, onPick: (VideoTrackInfo) -> Unit) {
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
          ui.frames[track.streamId],
          ui.ownerOf(track),
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
  maybeOwner: Person?,
  modifier: Modifier,
  scale: ContentScale,
  shape: Shape,
) {
  val description = stringResource(if (track.isScreen) R.string.call_video_screen else R.string.call_video_camera)
  Box(
    modifier
      .clip(shape)
      .background(Color.Black, shape)
      .border(
        if (maybeOwner?.isSpeaking == true) 3.dp else 1.dp,
        if (maybeOwner?.isSpeaking == true) SPEAKING_COLOR else Color.White.copy(alpha = 0.12f),
        shape,
      ),
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
    if (maybeOwner != null) NameLabel(maybeOwner.name, maybeOwner.isMuted, Modifier.align(Alignment.BottomStart))
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
        Icon(
          if (ui.isNotetakerPresent) Icons.Filled.Warning else Icons.Filled.Lock,
          contentDescription = null,
          tint = Color.White,
          modifier = Modifier.size(16.dp),
        )
        Text(
          stringResource(
            when {
              ui.isNotetakerPresent -> R.string.call_notetaker_badge
              isSecured -> R.string.call_secured
              else -> R.string.call_securing
            },
          ),
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
        CallEndReason.SERVER_LOST -> R.string.call_server_lost
        CallEndReason.DEVICE_LIMIT -> R.string.error_call_device_limit
        else -> R.string.call_ended
      },
    )
    CallPhase.CONNECTED -> {
      val count = state.participantCount.toInt()
      listOfNotNull(time, pluralStringResource(R.plurals.call_participants, count, count)).joinToString(" · ")
    }
  }
}

/** The hints that matter while talking: reconnecting, that nobody hears you, and a limited network. */
@Composable
private fun StatusHint(maybeState: CallStateInfo?) {
  val textRes = when {
    maybeState?.phase == CallPhase.RECONNECTING -> R.string.call_reconnecting
    maybeState?.isMuted == true && maybeState.phase == CallPhase.CONNECTED -> R.string.call_you_muted
    maybeState?.isOnLimitedNetwork == true && maybeState.phase == CallPhase.CONNECTED -> R.string.call_limited_network
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

/**
 * This device couldn't be added to the call's encryption (#1555): no media flows, so the user
 * hears why, how long until calling back may work (counted from when this showed), and can hang up.
 */
@Composable
private fun EncryptionFailureBanner(maybeState: CallStateInfo?, onHangUp: () -> Unit) {
  val retryAfterSecs = maybeState?.maybeEncryptionRetryAfterSecs ?: return
  val shownAt = remember(retryAfterSecs) { SystemClock.elapsedRealtime() }
  var now by remember { mutableLongStateOf(shownAt) }
  val minutesLeft = encryptionRetryMinutesLeft(retryAfterSecs.toLong(), shownAt, now)
  // Ticks only while a wait is shown.
  LaunchedEffect(shownAt, minutesLeft > 0) {
    while (minutesLeft > 0) {
      delay(1_000)
      now = SystemClock.elapsedRealtime()
    }
  }
  Surface(
    color = Color.Black.copy(alpha = 0.75f),
    contentColor = Color.White,
    shape = MaterialTheme.shapes.medium,
    modifier = Modifier.fillMaxWidth().padding(bottom = 12.dp).semantics { liveRegion = LiveRegionMode.Assertive },
  ) {
    Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
      Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.Top) {
        Icon(Icons.Filled.Warning, contentDescription = null, modifier = Modifier.size(20.dp))
        Text(stringResource(R.string.call_encryption_failed), style = MaterialTheme.typography.bodyMedium)
      }
      if (minutesLeft > 0) {
        Text(
          pluralStringResource(R.plurals.call_encryption_retry_in, minutesLeft, minutesLeft),
          style = MaterialTheme.typography.bodySmall,
        )
      }
      TextButton(onClick = onHangUp, modifier = Modifier.align(Alignment.End)) {
        Text(stringResource(R.string.call_hang_up), color = Color.White)
      }
    }
  }
}

/** Whole minutes until calling back may work, rounded up; 0 once it may. */
internal fun encryptionRetryMinutesLeft(retryAfterSecs: Long, shownAtMs: Long, nowMs: Long): Int {
  val leftMs = shownAtMs + retryAfterSecs * 1_000 - nowMs
  return if (leftMs <= 0) 0 else ((leftMs + 59_999) / 60_000).toInt()
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
        actions.maybeOnAddNotetaker?.let { onAddNotetaker ->
          DropdownMenuItem(
            text = { Text(stringResource(R.string.call_notetaker_add)) },
            leadingIcon = { Icon(Icons.Filled.Edit, contentDescription = null) },
            onClick = {
              isMenuOpen = false
              onAddNotetaker()
            },
          )
        }
        actions.maybeNotetakerRefusal?.let { refusal ->
          DropdownMenuItem(
            text = {
              Column {
                Text(stringResource(R.string.call_notetaker_add))
                Text(stringResource(refusal), style = MaterialTheme.typography.bodySmall)
              }
            },
            leadingIcon = { Icon(Icons.Filled.Edit, contentDescription = null) },
            enabled = false,
            onClick = {},
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
