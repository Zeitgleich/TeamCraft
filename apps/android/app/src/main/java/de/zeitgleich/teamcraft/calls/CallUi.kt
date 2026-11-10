package de.zeitgleich.teamcraft.calls

import android.graphics.Bitmap
import android.os.SystemClock
import android.provider.Settings
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.EnterTransition
import androidx.compose.animation.ExitTransition
import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.Spring
import androidx.compose.animation.core.VectorConverter
import androidx.compose.animation.core.animateDpAsState
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.snap
import androidx.compose.animation.core.spring
import androidx.compose.animation.core.tween
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
import androidx.compose.foundation.layout.BoxScope
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.asPaddingValues
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.plus
import androidx.compose.foundation.layout.safeDrawing
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
import androidx.compose.material.icons.filled.Edit
import androidx.compose.material.icons.filled.Lock
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material.icons.filled.Warning
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.DropdownMenuGroup
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.DropdownMenuPopup
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ExperimentalMaterial3ExpressiveApi
import androidx.compose.material3.FilledIconButton
import androidx.compose.material3.FilledTonalIconButton
import androidx.compose.material3.FilledTonalIconToggleButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.IconButtonDefaults
import androidx.compose.material3.IconButtonDefaults.IconButtonWidthOption
import androidx.compose.material3.LocalContentColor
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.MenuDefaults
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.State
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
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
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.draw.shadow
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.RectangleShape
import androidx.compose.ui.graphics.Shape
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.onClick
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.TextStyle
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
import de.zeitgleich.teamcraft.ui.avatarColor
import kotlin.math.roundToInt
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import uniffi.teamcraft_ffi.CallEndReason
import uniffi.teamcraft_ffi.CallPhase
import uniffi.teamcraft_ffi.CallStateInfo
import uniffi.teamcraft_ffi.VideoTrackInfo

/** The call's dark stage, behind everything on the call screens. */
internal val CALL_STAGE_COLOR = Color(0xFF0B0D10)

/** Hang up and decline, and answer: Google Phone's red and green, the same on every call screen. */
internal val HANG_UP_RED = Color(0xFFD93A3A)
internal val ANSWER_GREEN = Color(0xFF2E9E5B)

private val SELF_VIEW_WIDTH = 108.dp
private val SELF_VIEW_HEIGHT = 144.dp
private val SELF_VIEW_MARGIN = 12.dp
private val SELF_VIEW_ELEVATION = 6.dp
private val FILMSTRIP_TILE_WIDTH = 104.dp
private val FILMSTRIP_TILE_HEIGHT = 78.dp
private val TILE_SHAPE = RoundedCornerShape(20.dp)
private val FILMSTRIP_SHAPE = RoundedCornerShape(14.dp)
private val GRID_GAP = 6.dp
private val STAGE_MARGIN = 8.dp

/** The active speaker's tile: a thin outline in the accent, as Google Meet marks it. */
private val SPEAKER_OUTLINE = 2.dp

/** The one other person's avatar when nobody shows video (Google Phone's in-call screen). */
private val SOLO_AVATAR_SIZE = 144.dp

/** How far the speaking glow reaches beyond the avatar, at rest and at the pulse's peak. */
private val GLOW_REST = 6.dp
private val GLOW_PULSE = 10.dp
private val GLOW_RING = 3.dp
private const val GLOW_ALPHA = 0.28f
private const val GLOW_RING_ALPHA = 0.9f
private const val GLOW_PULSE_MILLIS = 700
private const val GLOW_FADE_MILLIS = 180

/** The gradients behind the header and the controls over video, so they read on any picture. */
private val SCRIM_TOP_HEIGHT = 180.dp
private val SCRIM_BOTTOM_HEIGHT = 220.dp
private const val SCRIM_ALPHA = 0.55f

/** The header's and the controls' height until they were measured once. */
private val TOP_CHROME_FALLBACK = 112.dp
private val BOTTOM_CHROME_FALLBACK = 88.dp

private val CONTROL_GAP = 8.dp

/** Up to this many tiles share the screen; more scroll. */
private const val GRID_FIXED_MAX = 6
private const val SCROLLING_TILE_RATIO = 0.8f

/** From this many people (you included) the header counts them; a 1:1 call doesn't. */
private const val GROUP_MIN_PEOPLE = 3
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
  /** A DM or group DM call: while nobody else is in it yet, it says "Calling…" (others ring). */
  val isDirectCall: Boolean = false,
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

private fun CallUiState.ownerOf(track: VideoTrackInfo, people: List<Person>): Person? =
  maybeState?.peers?.firstOrNull { it.deviceId == track.deviceId }?.let { peer ->
    people.firstOrNull { it.userId == peer.userId }
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
  val owners = cameras.associateWith { ui.ownerOf(it, people) }
  val tiles = people.map { person ->
    owners.entries.firstOrNull { it.value?.userId == person.userId }?.let { StageTile.Camera(it.key, person) } ?: StageTile.Avatar(person)
  }
  val unmatched = cameras.filter { owners[it] == null }.map { StageTile.Camera(it, null) }
  return (tiles + unmatched).ifEmpty { listOf(StageTile.Avatar(Person("", ui.name, isSpeaking = false, isRejoining = false))) }
}

/** How the stage shows the call. */
private sealed interface StageLayout {
  /** A shared screen as the spotlight, the other videos in a filmstrip. */
  data object Spotlight : StageLayout

  /** One other person with their camera on: their video edge to edge, as Google Meet's 1:1. */
  class FullBleed(val tile: StageTile.Camera) : StageLayout

  /** One other person (or the one called) without video: a large avatar, as Google Phone. */
  class Solo(val person: Person) : StageLayout

  /** Everybody as a tile of a grid (#1488). */
  class Grid(val tiles: List<StageTile>) : StageLayout
}

private fun stageLayout(ui: CallUiState): StageLayout {
  if (ui.tracks.any { it.isScreen }) return StageLayout.Spotlight
  val tiles = stageTiles(ui)
  return when (val only = tiles.singleOrNull()) {
    is StageTile.Camera -> StageLayout.FullBleed(only)
    is StageTile.Avatar -> StageLayout.Solo(only.person)
    null -> StageLayout.Grid(tiles)
  }
}

/** The one other person of a 1:1 call, whose mute the header shows; `null` in a group. */
private fun StageLayout.maybeOther(): Person? = when (this) {
  is StageLayout.Solo -> person.takeIf { it.userId.isNotEmpty() }
  is StageLayout.FullBleed -> tile.maybeOwner
  else -> null
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
  /** "Try again" on the encryption failure banner: rejoins the call in place (#1624). */
  val onRetryEncryption: () -> Unit = {},
  /** "Add notetaker" (e2ee.md §8.2): offered to whoever the policy lets ask it in. */
  val maybeOnAddNotetaker: (() -> Unit)? = null,
  /**
   * Why "Add notetaker" shows, but disabled, for whoever the policy keeps from asking it in (a
   * string resource; issue #1290).
   */
  val maybeNotetakerRefusal: Int? = null,
)

/** The room the header and the bottom (notices and controls) take, so the stage keeps clear of them. */
private class ChromeRoom(val top: Dp, val bottom: Dp)

/** The line under the call's name: its phase, the time, how many are in it, or why it ended. */
private class CallStatus(val text: String, val isTicking: Boolean)

/**
 * The in-call screen, as Google Meet and Google Phone lay it out: a dark edge-to-edge stage (the
 * one other person's video full-bleed, or their large avatar without video; a grid of tiles in a
 * group; a shared screen as a spotlight with a filmstrip), the name, status and a quiet encryption
 * indicator on top, Material 3 Expressive toggle buttons at the bottom, and the self-view as a
 * draggable floating picture. Over video, the header and controls sit on soft gradients and a tap
 * on the stage hides or shows them.
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
  val layout = stageLayout(ui)
  val status = callStatus(ui)
  val density = LocalDensity.current
  // Measured, not guessed, so tiles and the self-view never sit under the header, a notice or the
  // controls. Hidden chrome keeps its last size: the stage doesn't jump when it fades out.
  var topChromePx by remember { mutableIntStateOf(0) }
  var noticesPx by remember { mutableIntStateOf(0) }
  var controlsPx by remember { mutableIntStateOf(0) }
  val chrome = with(density) {
    ChromeRoom(
      top = if (topChromePx > 0) topChromePx.toDp() else TOP_CHROME_FALLBACK,
      bottom = noticesPx.toDp() + if (controlsPx > 0) controlsPx.toDp() else BOTTOM_CHROME_FALLBACK,
    )
  }
  CompositionLocalProvider(LocalContentColor provides MaterialTheme.colorScheme.onSurface) {
    Box(modifier.fillMaxSize().background(CALL_STAGE_COLOR)) {
      Stage(ui, layout, status, chrome, reduceMotion, actions.onToggleControls, Modifier.fillMaxSize())
      if (ui.tracks.isNotEmpty()) Scrims(ui.areControlsShown, reduceMotion)
      Column(Modifier.fillMaxSize().safeDrawingPadding()) {
        AnimatedVisibility(
          visible = ui.areControlsShown,
          enter = if (reduceMotion) EnterTransition.None else fadeIn() + slideInVertically { -it / 2 },
          exit = if (reduceMotion) ExitTransition.None else fadeOut() + slideOutVertically { -it / 2 },
        ) {
          TopBar(ui, layout, status, actions, Modifier.onSizeChanged { topChromePx = it.height })
        }
        Spacer(Modifier.weight(1f))
        // Notices sit above the controls, never over a tile's name.
        Column(
          Modifier.fillMaxWidth().padding(horizontal = 16.dp).onSizeChanged { noticesPx = it.height },
          horizontalAlignment = Alignment.CenterHorizontally,
        ) {
          StatusHint(ui.maybeState)
          EncryptionFailureBanner(ui.maybeState, actions.onRetryEncryption, actions.onHangUp)
          banner()
        }
        AnimatedVisibility(
          visible = ui.areControlsShown,
          enter = if (reduceMotion) EnterTransition.None else fadeIn() + slideInVertically { it / 2 },
          exit = if (reduceMotion) ExitTransition.None else fadeOut() + slideOutVertically { it / 2 },
        ) {
          ControlBar(ui, actions, Modifier.onSizeChanged { controlsPx = it.height })
        }
      }
      if (selfView != null) SelfViewCorner(selfView, chrome, reduceMotion)
    }
  }
}

@Composable
private fun Stage(
  ui: CallUiState,
  layout: StageLayout,
  status: CallStatus,
  chrome: ChromeRoom,
  reduceMotion: Boolean,
  onToggleControls: () -> Unit,
  modifier: Modifier,
) {
  val toggleLabel = stringResource(R.string.call_toggle_controls)
  // No ripple across the whole screen: a tap only shows or hides the chrome, as in Meet.
  Box(modifier.clickable(interactionSource = null, indication = null, onClickLabel = toggleLabel, onClick = onToggleControls)) {
    when (layout) {
      StageLayout.Spotlight -> SpotlightStage(ui, chrome)
      // Edge to edge, behind the system bars, cropped to fill; the header names the person.
      is StageLayout.FullBleed -> VideoTile(
        layout.tile.track,
        ui.frames[layout.tile.track.streamId],
        maybeOwner = null,
        modifier = Modifier.fillMaxSize(),
        scale = ContentScale.Crop,
        shape = RectangleShape,
      )
      is StageLayout.Solo -> SoloStage(ui, layout.person, status, chrome, reduceMotion)
      is StageLayout.Grid -> GridStage(ui, layout.tiles, chrome, reduceMotion)
    }
  }
}

@Composable
private fun SpotlightStage(ui: CallUiState, chrome: ChromeRoom) {
  var maybeSpotlight by rememberSaveable { mutableStateOf<Long?>(null) }
  // Shared screens first: they are what people look at.
  val ordered = ui.tracks.sortedByDescending { it.isScreen }
  val spotlight = ordered.firstOrNull { it.streamId.toLong() == maybeSpotlight } ?: ordered.firstOrNull() ?: return
  val filmstrip = ordered.filter { it != spotlight }
  val people = ui.people()
  VideoTile(spotlight, ui.frames[spotlight.streamId], ui.ownerOf(spotlight, people), Modifier.fillMaxSize(), ContentScale.Fit, RectangleShape)
  if (filmstrip.isNotEmpty()) {
    Filmstrip(filmstrip, ui, people, chrome.bottom, onPick = { maybeSpotlight = it.streamId.toLong() })
  }
}

/**
 * The 1:1 call without video, as Google Phone shows it: no card, a large avatar in the middle of
 * the stage with a soft glow while the person speaks, their name big below it and the call's
 * status under that.
 */
@Composable
private fun SoloStage(ui: CallUiState, person: Person, status: CallStatus, chrome: ChromeRoom, reduceMotion: Boolean) {
  val padding = animatedRoom(chrome, reduceMotion)
  Column(
    Modifier.fillMaxSize().safeDrawingPadding().padding(padding).padding(horizontal = 24.dp),
    verticalArrangement = Arrangement.Center,
    horizontalAlignment = Alignment.CenterHorizontally,
  ) {
    val name = person.name.ifEmpty { ui.name }
    SpeakingAvatar(person.userId, name, person.isSpeaking, SOLO_AVATAR_SIZE, reduceMotion, Modifier.alpha(if (person.isRejoining) 0.5f else 1f))
    Spacer(Modifier.size(24.dp))
    Text(
      name,
      style = MaterialTheme.typography.headlineMedium,
      textAlign = TextAlign.Center,
      maxLines = 2,
      overflow = TextOverflow.Ellipsis,
    )
    Spacer(Modifier.size(8.dp))
    StatusLine(status, person.takeIf { it.userId.isNotEmpty() }, MaterialTheme.typography.bodyLarge)
  }
}

/**
 * The call as a grid (#1488): a tile with rounded corners per person, a small gap between them,
 * their camera when it is on, their avatar otherwise, and the name on the tile; whoever speaks
 * has a thin outline. Up to six tiles share the screen; more scroll in two columns. The header,
 * notices and controls keep their room.
 */
@Composable
private fun GridStage(ui: CallUiState, tiles: List<StageTile>, chrome: ChromeRoom, reduceMotion: Boolean) {
  val columns = gridColumns(tiles.size)
  val room = animatedRoom(chrome, reduceMotion)
  val padding = PaddingValues(start = STAGE_MARGIN, end = STAGE_MARGIN) + room
  if (tiles.size > GRID_FIXED_MAX) {
    LazyVerticalGrid(
      columns = GridCells.Fixed(columns),
      modifier = Modifier.fillMaxSize(),
      // Edge to edge: the tiles scroll behind the system bars and the first and last keep clear of them.
      contentPadding = WindowInsets.safeDrawing.asPaddingValues() + padding,
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

/** The chrome's room as padding, eased when a notice comes or goes. */
@Composable
private fun animatedRoom(chrome: ChromeRoom, reduceMotion: Boolean): PaddingValues {
  val spec = if (reduceMotion) snap() else spring<Dp>(stiffness = Spring.StiffnessMediumLow)
  val top by animateDpAsState(chrome.top + GRID_GAP, spec, label = "chromeTop")
  val bottom by animateDpAsState(chrome.bottom + GRID_GAP, spec, label = "chromeBottom")
  return PaddingValues(top = top, bottom = bottom)
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
  BoxWithConstraints(
    modifier
      .alpha(if (person.isRejoining) 0.5f else 1f)
      .clip(TILE_SHAPE)
      .background(MaterialTheme.colorScheme.surfaceContainerHigh)
      .speakerOutline(person.isSpeaking, TILE_SHAPE),
    contentAlignment = Alignment.Center,
  ) {
    // As large as the tile allows, within reason: a lone tile's avatar isn't a poster.
    val size = (minOf(maxWidth, maxHeight) * 0.45f).coerceIn(48.dp, 128.dp)
    AvatarImage(person.userId, person.name, size)
    NameLabel(person.name, person.isMuted, Modifier.align(Alignment.BottomStart))
  }
}

/** The thin accent outline on the active speaker's tile; visible as a shape, not only a tint. */
@Composable
private fun Modifier.speakerOutline(isSpeaking: Boolean, shape: Shape): Modifier =
  if (isSpeaking) border(SPEAKER_OUTLINE, MaterialTheme.colorScheme.primary, shape) else this

/** A person's picture, or their initial on their colour. */
@Composable
private fun AvatarImage(userId: String, name: String, size: Dp, modifier: Modifier = Modifier) {
  UserAvatar(userId, modifier.size(size)) {
    Box(
      modifier.size(size).background(avatarColor(userId.ifEmpty { null }, name), CircleShape),
      contentAlignment = Alignment.Center,
    ) {
      Text(
        name.firstOrNull()?.uppercase() ?: "?",
        color = Color.White,
        fontSize = (size.value * 0.42f).sp,
        fontWeight = FontWeight.Medium,
      )
    }
  }
}

/**
 * A large avatar with a soft glow while the person speaks: a ring that is there as long as they
 * speak and a halo that breathes with it. The animation runs only while they speak, and not at
 * all with reduced motion (the ring then just shows); it is read in the draw phase only.
 */
@Composable
private fun SpeakingAvatar(
  userId: String,
  name: String,
  isSpeaking: Boolean,
  avatarSize: Dp,
  reduceMotion: Boolean,
  modifier: Modifier = Modifier,
) {
  val glow = MaterialTheme.colorScheme.primary
  val level = animateFloatAsState(
    if (isSpeaking) 1f else 0f,
    if (reduceMotion) snap() else tween(GLOW_FADE_MILLIS),
    label = "speakingLevel",
  )
  val maybePulse: State<Float>? = if (isSpeaking && !reduceMotion) {
    rememberInfiniteTransition(label = "speakingPulse").animateFloat(
      initialValue = 0f,
      targetValue = 1f,
      animationSpec = infiniteRepeatable(tween(GLOW_PULSE_MILLIS), RepeatMode.Reverse),
      label = "speakingPulse",
    )
  } else {
    null
  }
  Box(
    modifier
      .size(avatarSize + (GLOW_REST + GLOW_PULSE) * 2)
      .drawBehind {
        val strength = level.value
        if (strength == 0f) return@drawBehind
        val radius = avatarSize.toPx() / 2
        val reach = GLOW_REST.toPx() + GLOW_PULSE.toPx() * (maybePulse?.value ?: 0f)
        drawCircle(glow.copy(alpha = GLOW_ALPHA * strength), radius = radius + reach)
        drawCircle(glow.copy(alpha = GLOW_RING_ALPHA * strength), radius = radius + GLOW_RING.toPx(), style = Stroke(GLOW_RING.toPx()))
      },
    contentAlignment = Alignment.Center,
  ) {
    AvatarImage(userId, name, avatarSize)
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

@Composable
private fun Filmstrip(tracks: List<VideoTrackInfo>, ui: CallUiState, people: List<Person>, bottomRoom: Dp, onPick: (VideoTrackInfo) -> Unit) {
  Box(Modifier.fillMaxSize().safeDrawingPadding(), contentAlignment = Alignment.BottomCenter) {
    LazyRow(
      modifier = Modifier.fillMaxWidth().padding(bottom = bottomRoom + GRID_GAP),
      contentPadding = PaddingValues(horizontal = 16.dp),
      horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
      items(tracks, key = { it.streamId.toLong() }) { track ->
        val label = stringResource(R.string.call_video_spotlight)
        VideoTile(
          track,
          ui.frames[track.streamId],
          ui.ownerOf(track, people),
          Modifier.size(FILMSTRIP_TILE_WIDTH, FILMSTRIP_TILE_HEIGHT).clickable(onClickLabel = label) { onPick(track) },
          ContentScale.Crop,
          FILMSTRIP_SHAPE,
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
      .speakerOutline(maybeOwner?.isSpeaking == true, shape),
    contentAlignment = Alignment.Center,
  ) {
    if (maybeFrame == null) {
      Text(stringResource(R.string.call_video_waiting), color = Color.White.copy(alpha = 0.7f), style = MaterialTheme.typography.bodySmall)
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

/** The soft gradients behind the header and the controls over video; they go with the chrome. */
@Composable
private fun BoxScope.Scrims(isShown: Boolean, reduceMotion: Boolean) {
  val scrim = Color.Black.copy(alpha = SCRIM_ALPHA)
  AnimatedVisibility(
    visible = isShown,
    modifier = Modifier.align(Alignment.TopCenter),
    enter = if (reduceMotion) EnterTransition.None else fadeIn(),
    exit = if (reduceMotion) ExitTransition.None else fadeOut(),
  ) {
    Box(Modifier.fillMaxWidth().height(SCRIM_TOP_HEIGHT).background(Brush.verticalGradient(listOf(scrim, Color.Transparent))))
  }
  AnimatedVisibility(
    visible = isShown,
    modifier = Modifier.align(Alignment.BottomCenter),
    enter = if (reduceMotion) EnterTransition.None else fadeIn(),
    exit = if (reduceMotion) ExitTransition.None else fadeOut(),
  ) {
    Box(Modifier.fillMaxWidth().height(SCRIM_BOTTOM_HEIGHT).background(Brush.verticalGradient(listOf(Color.Transparent, scrim))))
  }
}

/**
 * The header: the call's name and status (in a 1:1 call without video, the stage shows both, as
 * Google Phone does), the quiet encryption indicator, and "Switch camera" at the end while the
 * camera is on, as Meet has it.
 */
@Composable
private fun TopBar(ui: CallUiState, layout: StageLayout, status: CallStatus, actions: CallUiActions, modifier: Modifier) {
  val maybeTitle = when (layout) {
    // The stage names the person; a channel's name only when it isn't theirs.
    is StageLayout.Solo -> ui.name.takeIf { layout.person.name.isNotEmpty() && it != layout.person.name }
    else -> ui.name
  }
  Box(modifier.fillMaxWidth().padding(horizontal = 4.dp, vertical = 4.dp)) {
    Column(
      Modifier.align(Alignment.TopCenter).padding(horizontal = 52.dp, vertical = 4.dp),
      horizontalAlignment = Alignment.CenterHorizontally,
    ) {
      maybeTitle?.let {
        Text(it, style = MaterialTheme.typography.titleLarge, maxLines = 1, overflow = TextOverflow.Ellipsis)
      }
      if (layout !is StageLayout.Solo) {
        StatusLine(status, layout.maybeOther(), MaterialTheme.typography.bodyMedium)
      }
      EncryptionIndicator(ui)
    }
    if (ui.isCameraOn) {
      IconButton(
        onClick = {
          actions.onInteraction()
          actions.onFlipCamera()
        },
        shapes = IconButtonDefaults.shapes(),
        modifier = Modifier.align(Alignment.TopEnd),
      ) {
        Icon(CallIcons.Cameraswitch, contentDescription = stringResource(R.string.call_camera_flip))
      }
    }
  }
}

/** The status line; with the other person of a 1:1 call muted, a crossed-out microphone before it. */
@Composable
private fun StatusLine(status: CallStatus, maybeOther: Person?, style: TextStyle) {
  if (status.text.isEmpty() && maybeOther?.isMuted != true) return
  val color = MaterialTheme.colorScheme.onSurfaceVariant
  Row(
    // The phase is announced when it changes; the running time isn't, every second.
    Modifier.then(if (status.isTicking) Modifier else Modifier.semantics { liveRegion = LiveRegionMode.Polite }),
    verticalAlignment = Alignment.CenterVertically,
    horizontalArrangement = Arrangement.spacedBy(6.dp),
  ) {
    if (maybeOther?.isMuted == true) {
      Icon(
        CallIcons.MicOff,
        contentDescription = stringResource(R.string.call_person_muted, maybeOther.name),
        tint = color,
        modifier = Modifier.size(18.dp),
      )
    }
    Text(status.text, style = style, color = color, maxLines = 2, textAlign = TextAlign.Center)
  }
}

/** The end-to-end encryption indicator: a small lock and a label that open the code; quiet, not a pill. */
@Composable
private fun EncryptionIndicator(ui: CallUiState) {
  val state = ui.maybeState ?: return
  val code = state.maybePrivacyCode ?: return
  var isSheetOpen by rememberSaveable { mutableStateOf(false) }
  val isSecured = state.isSecured
  val label = stringResource(R.string.call_encryption_open)
  TextButton(
    onClick = { isSheetOpen = true },
    modifier = Modifier.semantics {
      onClick(label = label) {
        isSheetOpen = true
        true
      }
    },
    colors = ButtonDefaults.textButtonColors(
      // The notetaker's notice must be seen: where the audio goes is not end-to-end encrypted.
      contentColor = if (ui.isNotetakerPresent) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurfaceVariant,
    ),
    contentPadding = PaddingValues(horizontal = 12.dp, vertical = 4.dp),
  ) {
    Icon(
      if (ui.isNotetakerPresent) Icons.Filled.Warning else Icons.Filled.Lock,
      contentDescription = null,
      modifier = Modifier.size(14.dp),
    )
    Spacer(Modifier.width(6.dp))
    Text(
      stringResource(
        when {
          ui.isNotetakerPresent -> R.string.call_notetaker_badge
          isSecured -> R.string.call_secured
          else -> R.string.call_securing
        },
      ),
      style = MaterialTheme.typography.labelMedium,
      textAlign = TextAlign.Center,
    )
  }
  if (isSheetOpen) EncryptionSheet(code) { isSheetOpen = false }
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

/**
 * "Connecting…", or why the call ended; connected: the time, and in a group of three or more how
 * many are in it. Before anybody else came: "Calling…" in a DM, "Waiting for others…" elsewhere.
 */
@Composable
private fun callStatus(ui: CallUiState): CallStatus {
  val state = ui.maybeState
  val others = state?.participantCount?.toInt() ?: 0
  // Once somebody was in, an empty call isn't "calling" anymore: it shows its time until it ends.
  var hasHadOthers by rememberSaveable(state?.conversationId) { mutableStateOf(false) }
  LaunchedEffect(others > 0) {
    if (others > 0) hasHadOthers = true
  }
  val time = ui.elapsedSeconds?.let { formatDuration(it) }
  val text = when (state?.phase) {
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
    CallPhase.CONNECTED -> when {
      others == 0 && !hasHadOthers -> stringResource(if (ui.isDirectCall) R.string.call_calling else R.string.call_waiting_for_others)
      others + 1 >= GROUP_MIN_PEOPLE ->
        listOfNotNull(time, pluralStringResource(R.plurals.call_people, others + 1, others + 1)).joinToString(" · ")
      else -> time.orEmpty()
    }
  }
  return CallStatus(text, isTicking = state?.phase == CallPhase.CONNECTED && time != null && (others > 0 || hasHadOthers))
}

/**
 * The hints that matter while talking: reconnecting, that nobody hears you, and a limited
 * network; a small floating pill above the controls, clear of the tiles.
 */
@Composable
private fun StatusHint(maybeState: CallStateInfo?) {
  val textRes = when {
    maybeState?.phase == CallPhase.RECONNECTING -> R.string.call_reconnecting
    maybeState?.isMuted == true && maybeState.phase == CallPhase.CONNECTED -> R.string.call_you_muted
    maybeState?.isOnLimitedNetwork == true && maybeState.phase == CallPhase.CONNECTED -> R.string.call_limited_network
    else -> null
  } ?: return
  Surface(
    color = MaterialTheme.colorScheme.inverseSurface,
    contentColor = MaterialTheme.colorScheme.inverseOnSurface,
    shape = CircleShape,
    shadowElevation = 3.dp,
    modifier = Modifier.padding(bottom = 12.dp).semantics { liveRegion = LiveRegionMode.Polite },
  ) {
    Text(
      stringResource(textRes),
      Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
      style = MaterialTheme.typography.labelLarge,
      textAlign = TextAlign.Center,
    )
  }
}

/**
 * This device couldn't be added to the call's encryption (#1555): no media flows, so the user
 * hears why, how long until calling back may work (counted from when this showed), and can hang up.
 */
@Composable
private fun EncryptionFailureBanner(maybeState: CallStateInfo?, onRetry: () -> Unit, onHangUp: () -> Unit) {
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
    color = MaterialTheme.colorScheme.surfaceContainerHigh,
    contentColor = MaterialTheme.colorScheme.onSurface,
    shape = MaterialTheme.shapes.large,
    modifier = Modifier.fillMaxWidth().padding(bottom = 12.dp).semantics { liveRegion = LiveRegionMode.Assertive },
  ) {
    Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
      Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.Top) {
        Icon(Icons.Filled.Warning, contentDescription = null, modifier = Modifier.size(20.dp), tint = MaterialTheme.colorScheme.error)
        Text(stringResource(R.string.call_encryption_failed), style = MaterialTheme.typography.bodyMedium)
      }
      if (minutesLeft > 0) {
        Text(
          pluralStringResource(R.plurals.call_encryption_retry_in, minutesLeft, minutesLeft),
          style = MaterialTheme.typography.bodySmall,
        )
      }
      Row(Modifier.align(Alignment.End), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        TextButton(onClick = onHangUp) {
          Text(stringResource(R.string.call_hang_up))
        }
        // Disabled until the SFU's wait has passed; the line above says how long.
        TextButton(onClick = onRetry, enabled = canRetryEncryption(minutesLeft)) {
          Icon(Icons.Filled.Refresh, contentDescription = null, modifier = Modifier.size(18.dp))
          Spacer(Modifier.width(6.dp))
          Text(stringResource(R.string.call_encryption_retry))
        }
      }
    }
  }
}

/** "Try again" works once the wait has passed. */
internal fun canRetryEncryption(minutesLeft: Int): Boolean = minutesLeft <= 0

/** Whole minutes until calling back may work, rounded up; 0 once it may. */
internal fun encryptionRetryMinutesLeft(retryAfterSecs: Long, shownAtMs: Long, nowMs: Long): Int {
  val leftMs = shownAtMs + retryAfterSecs * 1_000 - nowMs
  return if (leftMs <= 0) 0 else ((leftMs + 59_999) / 60_000).toInt()
}

/**
 * The controls, as in Google Meet and Google Phone with Material 3 Expressive: medium tonal icon
 * toggle buttons that morph from round to a rounded square when switched (microphone off, camera
 * off, speaker on), "More options", and a wide red hang-up button. A plain row on the stage, no
 * toolbar container: Meet draws its controls straight on the call, and the hang-up's width and
 * colour set it apart.
 */
@Composable
private fun ControlBar(ui: CallUiState, actions: CallUiActions, modifier: Modifier) {
  val state = ui.maybeState
  val isMuted = state?.isMuted == true
  Row(
    modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 16.dp),
    horizontalArrangement = Arrangement.spacedBy(CONTROL_GAP, Alignment.CenterHorizontally),
    verticalAlignment = Alignment.CenterVertically,
  ) {
    // Checked is the state the label names: "Mute" checked is muted, "Camera off" checked is off.
    CallToggle(
      icon = if (isMuted) CallIcons.MicOff else CallIcons.Mic,
      label = stringResource(R.string.call_mute),
      isChecked = isMuted,
      isOffSwitch = true,
    ) {
      actions.onInteraction()
      actions.onMute(it)
    }
    CallToggle(
      icon = if (ui.isCameraOn) CallIcons.Videocam else CallIcons.VideocamOff,
      label = stringResource(R.string.call_camera_off),
      isChecked = !ui.isCameraOn,
      isOffSwitch = true,
    ) {
      actions.onInteraction()
      actions.onCamera(!it)
    }
    CallToggle(
      icon = CallIcons.VolumeUp,
      label = stringResource(R.string.call_speaker),
      isChecked = ui.isSpeakerOn,
      isOffSwitch = false,
    ) {
      actions.onInteraction()
      actions.onSpeaker(it)
    }
    MoreButton(state?.isDeafened == true, actions)
    HangUpButton(actions.onHangUp)
  }
}

/**
 * One of the call's switches: Material's filled tonal icon toggle button at the medium size,
 * round when off and a rounded square when on. [isOffSwitch]: checked turns something off
 * (microphone, camera), shown in the error colour as Meet marks it, with the "off" haptic.
 */
@Composable
private fun CallToggle(icon: ImageVector, label: String, isChecked: Boolean, isOffSwitch: Boolean, onChange: (Boolean) -> Unit) {
  val haptics = LocalHapticFeedback.current
  val colors = if (isOffSwitch) {
    IconButtonDefaults.filledTonalIconToggleButtonColors(
      checkedContainerColor = MaterialTheme.colorScheme.error,
      checkedContentColor = MaterialTheme.colorScheme.onError,
    )
  } else {
    IconButtonDefaults.filledTonalIconToggleButtonColors()
  }
  FilledTonalIconToggleButton(
    checked = isChecked,
    onCheckedChange = { checked ->
      val isTurningOn = checked != isOffSwitch
      haptics.performHapticFeedback(if (isTurningOn) HapticFeedbackType.ToggleOn else HapticFeedbackType.ToggleOff)
      onChange(checked)
    },
    shapes = IconButtonDefaults.toggleableShapes(
      shape = IconButtonDefaults.mediumRoundShape,
      pressedShape = IconButtonDefaults.mediumPressedShape,
      checkedShape = IconButtonDefaults.mediumSelectedRoundShape,
    ),
    modifier = Modifier.size(IconButtonDefaults.mediumContainerSize()),
    colors = colors,
  ) {
    Icon(icon, contentDescription = label, modifier = Modifier.size(IconButtonDefaults.mediumIconSize))
  }
}

/** "More options" and its menu: Material 3 Expressive's grouped menu, every item with its icon. */
@OptIn(ExperimentalMaterial3ExpressiveApi::class)
@Composable
private fun MoreButton(isDeafened: Boolean, actions: CallUiActions) {
  var isMenuOpen by remember { mutableStateOf(false) }
  val entries = menuEntries(isDeafened, actions)
  Box {
    FilledTonalIconButton(
      onClick = {
        actions.onInteraction()
        isMenuOpen = true
      },
      shapes = IconButtonDefaults.shapes(
        shape = IconButtonDefaults.mediumRoundShape,
        pressedShape = IconButtonDefaults.mediumPressedShape,
      ),
      modifier = Modifier.size(IconButtonDefaults.mediumContainerSize()),
    ) {
      Icon(Icons.Filled.MoreVert, contentDescription = stringResource(R.string.call_more), modifier = Modifier.size(IconButtonDefaults.mediumIconSize))
    }
    DropdownMenuPopup(expanded = isMenuOpen, onDismissRequest = { isMenuOpen = false }) {
      DropdownMenuGroup(shapes = MenuDefaults.groupShape(index = 0, count = 1)) {
        entries.forEachIndexed { index, entry ->
          DropdownMenuItem(
            onClick = {
              isMenuOpen = false
              entry.onClick()
            },
            text = { Text(entry.text) },
            shape = MenuDefaults.itemShape(index = index, count = entries.size).shape,
            leadingIcon = { Icon(entry.icon, contentDescription = null) },
            supportingText = entry.maybeSupporting?.let { supporting -> { Text(supporting) } },
            enabled = entry.isEnabled,
          )
        }
      }
    }
  }
}

/** An item of the call's "More options" menu. */
private class MenuEntry(
  val text: String,
  val icon: ImageVector,
  val maybeSupporting: String? = null,
  val isEnabled: Boolean = true,
  val onClick: () -> Unit = {},
)

@Composable
private fun menuEntries(isDeafened: Boolean, actions: CallUiActions): List<MenuEntry> = buildList {
  add(
    MenuEntry(
      text = stringResource(if (isDeafened) R.string.call_undeafen else R.string.call_deafen),
      icon = if (isDeafened) CallIcons.Headset else CallIcons.HeadsetOff,
    ) { actions.onDeafen(!isDeafened) },
  )
  actions.maybeOnAddPeople?.let { onAddPeople ->
    add(MenuEntry(stringResource(R.string.call_add_people), CallIcons.PersonAdd, onClick = onAddPeople))
  }
  actions.maybeOnAddNotetaker?.let { onAddNotetaker ->
    add(MenuEntry(stringResource(R.string.call_notetaker_add), Icons.Filled.Edit, onClick = onAddNotetaker))
  }
  actions.maybeNotetakerRefusal?.let { refusal ->
    add(MenuEntry(stringResource(R.string.call_notetaker_add), Icons.Filled.Edit, maybeSupporting = stringResource(refusal), isEnabled = false))
  }
}

/** Hang up: Material's filled icon button, wide and red, as Google Phone's. */
@Composable
private fun HangUpButton(onHangUp: () -> Unit) {
  val haptics = LocalHapticFeedback.current
  FilledIconButton(
    onClick = {
      haptics.performHapticFeedback(HapticFeedbackType.Reject)
      onHangUp()
    },
    shapes = IconButtonDefaults.shapes(
      shape = IconButtonDefaults.mediumRoundShape,
      pressedShape = IconButtonDefaults.mediumPressedShape,
    ),
    modifier = Modifier.size(IconButtonDefaults.mediumContainerSize(IconButtonWidthOption.Wide)),
    colors = IconButtonDefaults.filledIconButtonColors(containerColor = HANG_UP_RED, contentColor = Color.White),
  ) {
    Icon(CallIcons.CallEnd, contentDescription = stringResource(R.string.call_hang_up), modifier = Modifier.size(IconButtonDefaults.mediumIconSize))
  }
}

/**
 * The self-view as a floating picture in a corner, clear of the system bars, the header and the
 * controls: drag it and it springs to the nearest corner, which it keeps.
 */
@Composable
private fun SelfViewCorner(selfView: @Composable (Modifier) -> Unit, chrome: ChromeRoom, reduceMotion: Boolean) {
  val scope = rememberCoroutineScope()
  var corner by rememberSaveable { mutableIntStateOf(TOP_END_CORNER) }
  BoxWithConstraints(Modifier.fillMaxSize().safeDrawingPadding()) {
    val density = LocalDensity.current
    val area = with(density) { Offset(maxWidth.toPx(), maxHeight.toPx()) }
    val pipSize = with(density) { Offset(SELF_VIEW_WIDTH.toPx(), SELF_VIEW_HEIGHT.toPx()) }
    val margin = with(density) { SELF_VIEW_MARGIN.toPx() }
    // Below the header, above the notices and controls.
    val top = with(density) { (chrome.top + SELF_VIEW_MARGIN).toPx() }
    val bottom = with(density) { (chrome.bottom + SELF_VIEW_MARGIN).toPx() }
    val corners = listOf(
      Offset(margin, top),
      Offset(area.x - pipSize.x - margin, top),
      Offset(margin, area.y - pipSize.y - bottom),
      Offset(area.x - pipSize.x - margin, area.y - pipSize.y - bottom),
    )
    val position = remember { Animatable(Offset.Zero, Offset.VectorConverter) }
    LaunchedEffect(area, top, bottom) { position.snapTo(corners[corner]) }
    selfView(
      Modifier
        .offset { IntOffset(position.value.x.roundToInt(), position.value.y.roundToInt()) }
        .size(SELF_VIEW_WIDTH, SELF_VIEW_HEIGHT)
        .shadow(SELF_VIEW_ELEVATION, TILE_SHAPE)
        .clip(TILE_SHAPE)
        .background(Color.Black)
        .pointerInput(area, top, bottom) {
          detectDragGestures(
            onDrag = { change, drag ->
              change.consume()
              scope.launch { position.snapTo(position.value + drag) }
            },
            onDragEnd = {
              val centre = position.value + Offset(pipSize.x / 2, pipSize.y / 2)
              corner = corners.indices.minBy { (corners[it] + Offset(pipSize.x / 2, pipSize.y / 2) - centre).getDistance() }
              val target = corners[corner]
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

/** The self-view's first corner: top end, as Meet places it. */
private const val TOP_END_CORNER = 1
