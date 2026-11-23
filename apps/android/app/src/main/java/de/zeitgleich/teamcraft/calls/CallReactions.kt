package de.zeitgleich.teamcraft.calls

import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.LinearEasing
import androidx.compose.animation.core.tween
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import de.zeitgleich.teamcraft.R
import uniffi.teamcraft_ffi.CallReactionInfo
import uniffi.teamcraft_ffi.callReactions

/** How long a reaction shows over the stage. */
private const val FLOAT_MILLIS = 3200

/** The most reactions floating at once; older ones make room. */
private const val MAX_FLOATING = 12

/** How far a reaction rises before it fades. */
private val RISE = 220.dp

/** Fractions of the reaction's time it fades in and starts fading out. */
private const val FADE_IN_END = 0.1f
private const val FADE_OUT_START = 0.75f

/** The user's own latest reaction (#2324): [count] tells a new press of the same one apart. */
internal class OwnReaction(val count: Long, val emoji: String)

/** One reaction rising over the stage. */
private class Floating(val key: Long, val emoji: String, val name: String, val xFraction: Float)

/**
 * The call's reactions (#2324): one tap sends one, and the picker stays open for more. The
 * reactions are emoji (user content); each is named for TalkBack.
 */
@Composable
internal fun ReactionPicker(isOpen: Boolean, onDismiss: () -> Unit, onPick: (String) -> Unit) {
  val choices = remember { callReactions() }
  DropdownMenu(expanded = isOpen, onDismissRequest = onDismiss) {
    Row(Modifier.padding(horizontal = 4.dp), horizontalArrangement = Arrangement.spacedBy(2.dp)) {
      choices.forEach { choice ->
        val label = stringResource(reactionNameRes(choice.reaction))
        IconButton(
          onClick = { onPick(choice.reaction) },
          modifier = Modifier.size(48.dp).semantics { contentDescription = label },
        ) {
          Text(choice.emoji, fontSize = 26.sp, modifier = Modifier.clearAndSetSemantics {})
        }
      }
    }
  }
}

/**
 * The reactions rising over the stage: the others' as the call reports them (each one above the
 * last id seen; the first look is the baseline), the user's own as they press them. With reduced
 * motion they fade in and out in place. Hidden from TalkBack: [CallSignalAnnouncer] says them.
 */
@Composable
internal fun FloatingReactions(
  reactions: List<CallReactionInfo>,
  own: OwnReaction?,
  names: Map<String, String>,
  reduceMotion: Boolean,
  modifier: Modifier,
) {
  val floating = remember { mutableStateListOf<Floating>() }
  var nextKey by remember { mutableLongStateOf(0L) }
  var lastSeen by remember { mutableStateOf<ULong?>(null) }
  var ownSeen by remember { mutableStateOf(own?.count) }
  val currentNames by rememberUpdatedState(names)
  fun push(emoji: String, name: String) {
    nextKey += 1
    if (floating.size >= MAX_FLOATING) floating.removeAt(0)
    floating.add(Floating(nextKey, emoji, name, spread(nextKey)))
  }
  LaunchedEffect(reactions) {
    val seen = lastSeen
    lastSeen = maxOf(reactions.maxOfOrNull { it.id } ?: 0u, seen ?: 0u)
    if (seen == null) return@LaunchedEffect
    reactions.filter { it.id > seen }.forEach { push(it.emoji, currentNames[it.userId].orEmpty()) }
  }
  LaunchedEffect(own?.count) {
    val pressed = own ?: return@LaunchedEffect
    if (pressed.count == ownSeen) return@LaunchedEffect
    ownSeen = pressed.count
    push(pressed.emoji, "")
  }
  BoxWithConstraints(modifier.clearAndSetSemantics {}) {
    val width = maxWidth
    floating.forEach { item ->
      key(item.key) {
        FloatingItem(item, reduceMotion, Modifier.align(Alignment.BottomStart).offset(x = width * item.xFraction)) {
          floating.remove(item)
        }
      }
    }
  }
}

@Composable
private fun FloatingItem(item: Floating, reduceMotion: Boolean, modifier: Modifier, onDone: () -> Unit) {
  val progress = remember { Animatable(0f) }
  val rise = with(LocalDensity.current) { RISE.toPx() }
  LaunchedEffect(Unit) {
    progress.animateTo(1f, tween(FLOAT_MILLIS, easing = LinearEasing))
    onDone()
  }
  Column(
    modifier.graphicsLayer {
      val value = progress.value
      // Centred on its spot; transform and alpha only, so it stays off the layout pass.
      translationX = -size.width / 2
      if (!reduceMotion) translationY = -rise * value
      alpha = fade(value)
    },
    horizontalAlignment = Alignment.CenterHorizontally,
  ) {
    Text(item.emoji, fontSize = 36.sp)
    if (item.name.isNotEmpty()) {
      Text(
        item.name,
        color = Color.White,
        style = MaterialTheme.typography.labelSmall,
        maxLines = 1,
        overflow = TextOverflow.Ellipsis,
        modifier = Modifier
          .widthIn(max = 140.dp)
          .background(Color.Black.copy(alpha = 0.55f), CircleShape)
          .padding(horizontal = 8.dp, vertical = 1.dp),
      )
    }
  }
}

/**
 * Says raised hands and the others' reactions to TalkBack (#2324): its own polite live region, in
 * the tree before its first text. The first look is the baseline, not news; a burst of reactions
 * is counted, not listed.
 */
@Composable
internal fun CallSignalAnnouncer(raisedHandUserIds: List<String>, reactions: List<CallReactionInfo>, names: Map<String, String>) {
  val resources = LocalContext.current.resources
  var knownHands by remember { mutableStateOf<Set<String>?>(null) }
  var lastReaction by remember { mutableStateOf<ULong?>(null) }
  var message by remember { mutableStateOf("") }
  var flip by remember { mutableStateOf(false) }
  val currentNames by rememberUpdatedState(names)
  val hands = raisedHandUserIds.toSet()
  LaunchedEffect(hands, reactions) {
    val handsBefore = knownHands
    knownHands = hands
    val seen = lastReaction
    lastReaction = maxOf(reactions.maxOfOrNull { it.id } ?: 0u, seen ?: 0u)
    val someone = resources.getString(R.string.call_recording_someone)
    val nameOf = { id: String -> currentNames[id]?.takeIf { it.isNotEmpty() } ?: someone }
    val newHands = if (handsBefore == null) emptyList() else (hands - handsBefore).sorted()
    val newReactions = if (seen == null) emptyList() else reactions.filter { it.id > seen }
    val handsText = when {
      newHands.isEmpty() -> null
      newHands.size > ANNOUNCE_MAX_NAMED ->
        resources.getQuantityString(R.plurals.call_announce_hand_raised_many, newHands.size, newHands.size)
      else -> resources.getString(R.string.call_announce_hand_raised, newHands.joinToString(", ") { nameOf(it) })
    }
    val reactionsText = when {
      newReactions.isEmpty() -> null
      newReactions.size > ANNOUNCE_MAX_NAMED ->
        resources.getQuantityString(R.plurals.call_announce_reacted_many, newReactions.size, newReactions.size)
      else -> resources.getString(
        R.string.call_announce_reacted,
        newReactions.joinToString(", ") { "${nameOf(it.userId)} (${resources.getString(reactionNameRes(it.reaction))})" },
      )
    }
    val text = listOfNotNull(handsText, reactionsText).joinToString(". ")
    if (text.isEmpty()) return@LaunchedEffect
    // The same words twice in a row still change the region.
    flip = !flip
    message = if (flip) "$text​" else text
  }
  Box(
    Modifier.size(1.dp).semantics {
      liveRegion = LiveRegionMode.Polite
      contentDescription = message
    },
  )
}

/** The reaction's spoken name, by its stable name from the core. */
internal fun reactionNameRes(reaction: String): Int = when (reaction) {
  "thumbs_up" -> R.string.call_reaction_thumbs_up
  "applause" -> R.string.call_reaction_applause
  "laugh" -> R.string.call_reaction_laugh
  "heart" -> R.string.call_reaction_heart
  "celebrate" -> R.string.call_reaction_celebrate
  "surprise" -> R.string.call_reaction_surprise
  else -> R.string.call_reaction_other
}

/** The emoji of a reaction by its stable name; `null` for one the core doesn't offer. */
internal fun reactionEmoji(reaction: String): String? = callReactions().firstOrNull { it.reaction == reaction }?.emoji

/** Spreads reactions across the stage (10 … 90 %), so a burst doesn't stack in one column. */
private fun spread(key: Long): Float = (10 + (key * 61_803L).mod(80L)) / 100f

private fun fade(value: Float): Float = when {
  value < FADE_IN_END -> value / FADE_IN_END
  value > FADE_OUT_START -> (1f - value) / (1f - FADE_OUT_START)
  else -> 1f
}
