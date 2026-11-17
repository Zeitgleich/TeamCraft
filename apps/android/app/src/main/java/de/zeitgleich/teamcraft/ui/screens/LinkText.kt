package de.zeitgleich.teamcraft.ui.screens

import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.text.InlineTextContent
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.input.pointer.PointerEventPass
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.LinkAnnotation
import androidx.compose.ui.text.TextLayoutResult
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import de.zeitgleich.teamcraft.ui.Detected
import de.zeitgleich.teamcraft.ui.DetectedKind
import de.zeitgleich.teamcraft.ui.detectedKindOfUri
import kotlinx.coroutines.withTimeoutOrNull

/**
 * True while a finger rests on a link of a message's text (#2048). The message row's own long-press
 * checks it, so a long-press on a link opens the link's menu and not the message's.
 */
internal object LinkPress {
  @Volatile var isActive = false
}

/**
 * Message text whose web links, phone numbers, e-mail and postal addresses open a menu (open or call,
 * copy, share or add to contacts) on a long-press on the link itself (#2047, #2048). A tap still
 * follows the link, and a long-press anywhere else reaches the message's own menu.
 */
@Composable
internal fun LinkText(
  text: AnnotatedString,
  style: TextStyle,
  modifier: Modifier = Modifier,
  inlineContent: Map<String, InlineTextContent> = emptyMap(),
) {
  val haptics = LocalHapticFeedback.current
  var maybeLayout by remember { mutableStateOf<TextLayoutResult?>(null) }
  var maybeOpen by remember { mutableStateOf<Detected?>(null) }
  val links = remember(text) { menuLinks(text) }
  val pressModifier = if (links.isEmpty()) {
    Modifier
  } else {
    Modifier.pointerInput(links) {
      awaitEachGesture {
        // The initial pass sees the touch before the row and the link: nothing is consumed unless a
        // long-press on a link is certain, so a tap on the link and a long-press elsewhere go on as usual.
        val down = awaitFirstDown(requireUnconsumed = false, pass = PointerEventPass.Initial)
        val hit = maybeLayout?.let { linkAt(it, links, down.position) } ?: return@awaitEachGesture
        LinkPress.isActive = true
        try {
          val slop = viewConfiguration.touchSlop
          val ended = withTimeoutOrNull(viewConfiguration.longPressTimeoutMillis) {
            while (true) {
              val event = awaitPointerEvent(PointerEventPass.Initial)
              val change = event.changes.firstOrNull { it.id == down.id } ?: return@withTimeoutOrNull true
              if (!change.pressed || (change.position - down.position).getDistance() > slop) return@withTimeoutOrNull true
            }
            @Suppress("UNREACHABLE_CODE")
            true
          }
          if (ended == null) {
            haptics.performHapticFeedback(HapticFeedbackType.LongPress)
            maybeOpen = hit
            // The link's own tap must not fire on release.
            do {
              val event = awaitPointerEvent(PointerEventPass.Initial)
              event.changes.forEach { it.consume() }
            } while (event.changes.any { it.pressed })
          }
        } finally {
          LinkPress.isActive = false
        }
      }
    }
  }
  Text(
    text,
    modifier = modifier.then(pressModifier),
    style = style,
    inlineContent = inlineContent,
    onTextLayout = { maybeLayout = it },
  )
  maybeOpen?.let { detected ->
    LinkMenu(detected, onDismiss = { maybeOpen = null })
  }
}

@Composable
private fun LinkMenu(detected: Detected, onDismiss: () -> Unit) {
  Dialog(onDismissRequest = onDismiss) {
    Surface(shape = MaterialTheme.shapes.extraLarge, tonalElevation = 6.dp) {
      Column(Modifier.padding(vertical = 8.dp)) {
        DetectedRows(listOf(detected), onDone = onDismiss)
      }
    }
  }
}

/** The ranges of [text] that have a menu, with what to act on. */
private fun menuLinks(text: AnnotatedString): List<Pair<IntRange, Detected>> =
  text.getLinkAnnotations(0, text.length).mapNotNull { range ->
    val tag = (range.item as? LinkAnnotation.Clickable)?.tag ?: return@mapNotNull null
    val kind = detectedKindOfUri(tag) ?: return@mapNotNull null
    // A web link's value is its URL (its label may be shortened); the others, the text as written.
    val value = if (kind == DetectedKind.WEB) tag else text.text.substring(range.start, range.end)
    val span = range.start until range.end
    span to Detected(kind, span, value)
  }

private fun linkAt(layout: TextLayoutResult, links: List<Pair<IntRange, Detected>>, position: Offset): Detected? {
  if (layout.layoutInput.text.isEmpty()) return null
  val line = layout.getLineForVerticalPosition(position.y)
  // Past a line's end, the offset lookup would answer with the line's last character.
  if (position.x < layout.getLineLeft(line) || position.x > layout.getLineRight(line)) return null
  val offset = layout.getOffsetForPosition(position)
  return links.firstOrNull { offset in it.first }?.second
}
