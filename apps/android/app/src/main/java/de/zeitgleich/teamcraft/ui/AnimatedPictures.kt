package de.zeitgleich.teamcraft.ui

import android.graphics.ImageDecoder
import android.graphics.drawable.Animatable2
import android.graphics.drawable.AnimatedImageDrawable
import android.graphics.drawable.Drawable
import android.os.Build
import android.provider.Settings
import android.widget.ImageView
import androidx.annotation.RequiresApi
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.compositionLocalOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LifecycleResumeEffect
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.compose.currentStateAsState
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.FileInfo
import java.io.File
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/** Largest animated file a tile fetches by itself; a bigger one moves in the viewer (as on the web). */
const val MAX_AUTO_ANIMATION_BYTES = 16L * 1024 * 1024

/** A file with more pixels a frame isn't animated: it reaches the decoder from others, without a tap. */
const val MAX_ANIMATION_PIXELS = 16_000_000L

/** What tiles fetch by themselves per run of the app; after it a tap fetches, so a channel can't fill the cache. */
const val MAX_AUTO_FETCH_BYTES_PER_RUN = 256L * 1024 * 1024

/** How long a tile has to stay in the list before it fetches by itself: fast scrolling fetches nothing. */
const val AUTO_FETCH_SETTLE_MS = 300L

/** The shade under the play button and the format badge, and the button's size (a 44 dp target). */
private const val CONTROL_SCRIM = 0.55f
private val CONTROL_SIZE = 44.dp
private val CONTROL_ICON_SIZE = 20.dp

/** When animated images in the conversation move (#201). Phones have no hover to wait for. */
enum class AnimatedImagesMode {
  AUTOPLAY,
  ON_TAP,
  ;

  /** The system's "remove animations" changes the default: nothing starts by itself then. */
  fun effective(isMotionReduced: Boolean): AnimatedImagesMode = if (isMotionReduced) ON_TAP else this
}

/** What the user did to one image in the current view; it wins over the setting. */
enum class PlayChoice { UNSET, PLAYING, PAUSED }

/** Whether an image should move, as far as the setting and the user are concerned. */
fun wantsToMove(mode: AnimatedImagesMode, choice: PlayChoice): Boolean =
  when (choice) {
    PlayChoice.PLAYING -> true
    PlayChoice.PAUSED -> false
    PlayChoice.UNSET -> mode == AnimatedImagesMode.AUTOPLAY
  }

/** Whether a frame of this size may be animated. */
fun isWithinAnimationLimit(width: Int, height: Int): Boolean = width.toLong() * height <= MAX_ANIMATION_PIXELS

/**
 * `stop()` also reports an end, on a later turn of the main thread. Only what was meant to go on and
 * no longer runs reached the end of the file's loops.
 */
fun isRealEnd(wantsPlaying: Boolean, isRunning: Boolean): Boolean = wantsPlaying && !isRunning

/** What tiles may still fetch by themselves. */
class AutoFetchBudget(private val maxBytes: Long) {
  private var spentBytes = 0L

  /** Takes `bytes` from the budget; `false` when they don't fit, and then nothing is taken. */
  @Synchronized
  fun take(bytes: Long): Boolean {
    if (spentBytes + bytes > maxBytes) return false
    spentBytes += bytes
    return true
  }
}

val animationFetchBudget = AutoFetchBudget(MAX_AUTO_FETCH_BYTES_PER_RUN)

/** Formats that always move: the tile offers play and fetches the file when it is to move. */
fun isAnimationType(mediaType: String): Boolean = mediaTypeEssence(mediaType) in setOf("image/gif", "image/apng")

/** Formats that may move: only the decoder can tell, once the file is here. */
fun mayBeAnimated(mediaType: String): Boolean =
  isAnimationType(mediaType) || mediaTypeEssence(mediaType) in setOf("image/webp", "image/avif")

/** The short label on a still animation, like the other platforms': GIF, WEBP, APNG, AVIF. */
fun animationLabel(mediaType: String): String = mediaTypeEssence(mediaType).substringAfter('/').uppercase()

/** The media type without parameters, in lowercase. Senders write whatever they like. */
private fun mediaTypeEssence(mediaType: String): String = mediaType.substringBefore(';').trim().lowercase()

/**
 * What a conversation's tiles need to know: this device's mode, whether something covers them (the
 * image viewer), and how to fetch a file. Set by the feed.
 */
data class FeedMotion(
  val mode: AnimatedImagesMode = AnimatedImagesMode.ON_TAP,
  val isCovered: Boolean = false,
  val fetchesNow: () -> Boolean = { false },
  val fetch: (FileInfo) -> Unit = {},
)

val LocalFeedMotion = compositionLocalOf { FeedMotion() }

/** The system's "remove animations", read again whenever the app comes back to the front. */
@Composable
fun rememberMotionReduced(): Boolean {
  val resolver = LocalContext.current.contentResolver
  fun read() = Settings.Global.getFloat(resolver, Settings.Global.ANIMATOR_DURATION_SCALE, 1f) == 0f
  var isReduced by remember(resolver) { mutableStateOf(read()) }
  LifecycleResumeEffect(resolver) {
    isReduced = read()
    onPauseOrDispose {}
  }
  return isReduced
}

/** Whether the app is in front: nothing moves behind the lock screen or in the background. */
@Composable
fun isAppResumed(): Boolean {
  val lifecycleState by LocalLifecycleOwner.current.lifecycle.currentStateAsState()
  return lifecycleState.isAtLeast(Lifecycle.State.RESUMED)
}

/** How reading a file as an animation went. */
sealed interface AnimationRead {
  /** Nothing read yet: no file, or it is being read. */
  data object Waiting : AnimationRead

  /** It doesn't move here: one frame, unreadable, over the limit, or a system older than Android 9. */
  data object Still : AnimationRead

  data class Moving(val drawable: AnimatedImageDrawable) : AnimationRead
}

/**
 * The file as an animation at about `maxSidePx`. The drawable only holds the frames around the one
 * on screen, and only works while it is drawn.
 */
@Composable
fun rememberAnimation(maybePath: String?, maxSidePx: Int): AnimationRead {
  val read by produceState<AnimationRead>(AnimationRead.Waiting, maybePath, maxSidePx) {
    value = when {
      maybePath == null -> AnimationRead.Waiting
      Build.VERSION.SDK_INT < Build.VERSION_CODES.P -> AnimationRead.Still
      else -> decodeAnimation(maybePath, maxSidePx)?.let { AnimationRead.Moving(it) } ?: AnimationRead.Still
    }
  }
  return read
}

@RequiresApi(Build.VERSION_CODES.P)
private suspend fun decodeAnimation(path: String, maxSidePx: Int): AnimatedImageDrawable? =
  withContext(Dispatchers.IO) {
    runCatching {
      val file = File(path)
      // The core keeps the most recently used downloads when it trims its folder.
      file.setLastModified(System.currentTimeMillis())
      ImageDecoder.decodeDrawable(ImageDecoder.createSource(file)) { decoder, info, _ ->
        // Thrown out of the header listener before any frame is decoded; `runCatching` takes it.
        require(isWithinAnimationLimit(info.size.width, info.size.height)) { "too many pixels" }
        decoder.setTargetSampleSize(sampleSize(info.size.width, info.size.height, maxSidePx))
      } as? AnimatedImageDrawable
    }.getOrNull()
  }

/** Whether the view still means the drawable to run; read when it reports an end. */
private class PlaybackGate {
  var wantsPlaying = false
}

/**
 * An animation on screen: stopped it keeps its frame. Leaving the composition (scrolled away) stops
 * it for good. `onEnded` tells that a file with a limited number of loops finished, not that
 * somebody stopped it.
 */
@RequiresApi(Build.VERSION_CODES.P)
@Composable
fun AnimationView(
  drawable: AnimatedImageDrawable,
  isPlaying: Boolean,
  contentScale: ContentScale,
  modifier: Modifier = Modifier,
  onEnded: () -> Unit = {},
) {
  val currentOnEnded by rememberUpdatedState(onEnded)
  val gate = remember(drawable) { PlaybackGate() }
  DisposableEffect(drawable) {
    drawable.registerAnimationCallback(
      object : Animatable2.AnimationCallback() {
        override fun onAnimationEnd(ended: Drawable?) {
          if (isRealEnd(gate.wantsPlaying, drawable.isRunning)) currentOnEnded()
        }
      },
    )
    onDispose {
      gate.wantsPlaying = false
      // The callback stays registered, it dies with the drawable: `stop()` posts a walk over the
      // callback list, and unregistering the last one nulls the list, which crashes that walk.
      if (drawable.isRunning) drawable.stop()
    }
  }
  AndroidView(
    factory = { context ->
      ImageView(context).apply {
        scaleType = if (contentScale == ContentScale.Crop) ImageView.ScaleType.CENTER_CROP else ImageView.ScaleType.FIT_CENTER
        importantForAccessibility = ImageView.IMPORTANT_FOR_ACCESSIBILITY_NO
      }
    },
    update = { view ->
      if (view.drawable !== drawable) view.setImageDrawable(drawable)
      gate.wantsPlaying = isPlaying
      if (isPlaying && !drawable.isRunning) drawable.start()
      if (!isPlaying && drawable.isRunning) drawable.stop()
    },
    onRelease = { view -> view.setImageDrawable(null) },
    modifier = modifier,
  )
}

/** The button that stops or starts an animated image: a 44 dp target, a tap on it and nothing else. */
@Composable
fun AnimationButton(isMoving: Boolean, onClick: () -> Unit, modifier: Modifier = Modifier) {
  val label = stringResource(if (isMoving) R.string.animation_pause else R.string.animation_play)
  Box(
    modifier
      .size(CONTROL_SIZE)
      .clickable(onClickLabel = label, onClick = onClick),
    contentAlignment = Alignment.Center,
  ) {
    Box(
      Modifier.size(CONTROL_SIZE - 12.dp).background(Color.Black.copy(alpha = CONTROL_SCRIM), CircleShape),
      contentAlignment = Alignment.Center,
    ) {
      Icon(
        painterResource(if (isMoving) R.drawable.ic_pause else R.drawable.ic_play_arrow),
        contentDescription = label,
        tint = Color.White,
        modifier = Modifier.size(CONTROL_ICON_SIZE),
      )
    }
  }
}

/** Says there is motion behind a still frame, or that its file didn't come. */
@Composable
fun AnimationBadge(text: String, modifier: Modifier = Modifier) {
  Text(
    text,
    color = Color.White,
    style = MaterialTheme.typography.labelSmall,
    modifier = modifier
      .padding(6.dp)
      .background(Color.Black.copy(alpha = CONTROL_SCRIM), RoundedCornerShape(4.dp))
      .padding(horizontal = 6.dp, vertical = 1.dp)
      .clearAndSetSemantics {},
  )
}
