package de.zeitgleich.teamcraft.ui

import android.text.format.DateUtils
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.ChatActions
import de.zeitgleich.teamcraft.core.Message
import de.zeitgleich.teamcraft.core.Poll
import kotlinx.coroutines.delay
import uniffi.teamcraft_ffi.pollChoicesAfterPressing
import uniffi.teamcraft_ffi.pollSharePercent

/** Names listed under an option before "and N more". */
private const val MAX_LISTED_VOTERS = 5

/** After the closing time, how long the card waits before the core counts again. */
private const val CLOSING_MARGIN_MS = 500L

/** Longest wait for a closing time while the card is shown; a later one is counted on a change. */
private const val MAX_CLOSING_WAIT_MS = 24L * 60 * 60 * 1000

/**
 * A poll in a message (#205, e2ee.md §4.12), as web and desktop show it: the question, each option
 * with its count and share in words and a bar that only repeats them, voting in place. A single
 * choice picks or withdraws; with several answers each option toggles. Counts stay hidden until
 * the user voted or the poll closed when the poll says so. The poll's sender can close it; a
 * closing time ends it by itself (the server's clock decides what counts).
 */
@Composable
fun PollCard(
  viewModel: MainViewModel,
  message: Message,
  poll: Poll,
  /** The user may vote here (the conversation takes their messages). */
  canVote: Boolean,
  colors: PollColors,
  nameOf: (String) -> String,
) {
  val haptics = LocalHapticFeedback.current
  var isConfirmingClose by remember { mutableStateOf(false) }
  val canVoteNow = canVote && poll.isKnownChoice && !poll.isClosed
  val leader = if (poll.isClosed && poll.areResultsShown) leaderOf(poll) else null
  RecountAtClosing(viewModel, poll)
  Column(
    Modifier
      .widthIn(min = 220.dp)
      .padding(vertical = 4.dp),
    verticalArrangement = Arrangement.spacedBy(8.dp),
  ) {
    Row(verticalAlignment = Alignment.Top, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
      Icon(
        painterResource(R.drawable.ic_bar_chart),
        contentDescription = null,
        tint = colors.accent,
        modifier = Modifier.size(20.dp).padding(top = 2.dp),
      )
      Text(
        poll.question,
        style = MaterialTheme.typography.titleSmall,
        fontWeight = FontWeight.SemiBold,
        color = colors.content,
      )
    }
    Text(modeText(poll), style = MaterialTheme.typography.labelSmall, color = colors.muted)
    poll.options.forEachIndexed { index, option ->
      val share = if (poll.areResultsShown) pollSharePercent(option.count.toUInt(), poll.voterCount.toUInt()).toInt() else 0
      val maybeChoices = if (canVoteNow) {
        pollChoicesAfterPressing(
          poll.choice,
          poll.myChoices.map { it.toUInt() },
          poll.options.size.toUInt(),
          poll.isClosed,
          index.toUInt(),
        )
      } else {
        null
      }
      PollOptionRow(
        text = option.text,
        isChosen = option.isChosen,
        isLeader = leader == index,
        isMultiple = poll.isMultipleChoice,
        maybeCount = if (poll.areResultsShown) option.count else null,
        share = share,
        voters = if (poll.areVotersShown) votersLine(option.voters, nameOf) else null,
        colors = colors,
        maybeOnPress = maybeChoices?.let { choices ->
          {
            haptics.performHapticFeedback(HapticFeedbackType.Confirm)
            viewModel.act(ChatActions.voteInPoll(message.conversation, message.id, choices.map { it.toInt() }))
          }
        },
      )
    }
    if (!poll.areResultsShown) {
      Text(
        stringResource(if (poll.results == "after_vote") R.string.poll_results_hidden_vote else R.string.poll_results_hidden_close),
        style = MaterialTheme.typography.bodySmall,
        color = colors.muted,
      )
    }
    if (!poll.isKnownChoice) {
      Text(stringResource(R.string.poll_unknown_choice), style = MaterialTheme.typography.bodySmall, color = colors.muted)
    }
    PollFooter(poll, colors, onClose = { isConfirmingClose = true })
  }
  if (isConfirmingClose) {
    AlertDialog(
      onDismissRequest = { isConfirmingClose = false },
      title = { Text(stringResource(R.string.poll_close)) },
      text = { Text(stringResource(R.string.poll_close_confirm)) },
      confirmButton = {
        TextButton(onClick = {
          isConfirmingClose = false
          viewModel.act(ChatActions.closePoll(message.conversation, message.id))
        }) { Text(stringResource(R.string.poll_close)) }
      },
      dismissButton = { TextButton(onClick = { isConfirmingClose = false }) { Text(stringResource(R.string.cancel)) } },
    )
  }
}

/** The bubble's colours the card draws with: text, secondary text and the accent of bars and marks. */
class PollColors(val content: Color, val muted: Color, val accent: Color)

/** One option: mark, text, count and share in words, the bar, and who chose it. */
@Composable
private fun PollOptionRow(
  text: String,
  isChosen: Boolean,
  isLeader: Boolean,
  isMultiple: Boolean,
  maybeCount: Int?,
  share: Int,
  voters: String?,
  colors: PollColors,
  maybeOnPress: (() -> Unit)?,
) {
  val shape = RoundedCornerShape(12.dp)
  val countText = maybeCount?.let { count -> stringResource(R.string.poll_option_count, count, share) }
  val state = maybeCount?.let { count ->
    pluralStringResource(R.plurals.poll_option_votes, count, count, share)
  }
  val fill by animateFloatAsState(targetValue = share / 100f, label = "poll share")
  val mark = when {
    isMultiple && isChosen -> R.drawable.ic_check_box
    isMultiple -> R.drawable.ic_check_box_outline_blank
    isChosen -> R.drawable.ic_check_circle
    else -> R.drawable.ic_circle
  }
  Column {
    Column(
      Modifier
        .fillMaxWidth()
        .clip(shape)
        .border(1.dp, if (isChosen) colors.accent else colors.muted.copy(alpha = 0.4f), shape)
        .background(if (isChosen) colors.accent.copy(alpha = 0.12f) else Color.Transparent)
        .toggleable(
          value = isChosen,
          enabled = maybeOnPress != null,
          role = if (isMultiple) Role.Checkbox else Role.RadioButton,
          onValueChange = { maybeOnPress?.invoke() },
        )
        .semantics(mergeDescendants = true) { state?.let { stateDescription = it } }
        .padding(horizontal = 12.dp, vertical = 10.dp),
      verticalArrangement = Arrangement.spacedBy(6.dp),
    ) {
      Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
        Icon(painterResource(mark), contentDescription = null, tint = if (isChosen) colors.accent else colors.muted, modifier = Modifier.size(20.dp))
        Text(
          text,
          style = MaterialTheme.typography.bodyLarge,
          fontWeight = if (isLeader) FontWeight.SemiBold else null,
          color = colors.content,
          modifier = Modifier.weight(1f),
        )
        countText?.let {
          Text(it, style = MaterialTheme.typography.labelMedium, color = colors.muted, modifier = Modifier.clearAndSetSemantics {})
        }
      }
      if (maybeCount != null) {
        Box(
          Modifier
            .fillMaxWidth()
            .height(4.dp)
            .clip(RoundedCornerShape(2.dp))
            .background(colors.muted.copy(alpha = 0.2f))
            .clearAndSetSemantics {},
        ) {
          Box(
            Modifier
              .fillMaxWidth()
              .height(4.dp)
              .graphicsLayer {
                scaleX = fill
                transformOrigin = androidx.compose.ui.graphics.TransformOrigin(0f, 0.5f)
              }
              .background(colors.accent),
          )
        }
      }
    }
    voters?.let {
      Text(
        it,
        style = MaterialTheme.typography.labelSmall,
        color = colors.muted,
        modifier = Modifier.padding(start = 42.dp, top = 4.dp),
      )
    }
  }
}

/** "3 votes · Closed" or "3 votes · Ends 18 Nov, 15:00", and "Close poll" for its sender. */
@Composable
private fun PollFooter(poll: Poll, colors: PollColors, onClose: () -> Unit) {
  val context = LocalContext.current
  val parts = buildList {
    add(pluralStringResource(R.plurals.poll_votes, poll.voterCount, poll.voterCount))
    if (poll.isClosed) {
      add(stringResource(R.string.poll_closed))
    } else {
      poll.maybeClosesAt?.let { closesAt ->
        val time = DateUtils.formatDateTime(
          context,
          closesAt,
          DateUtils.FORMAT_SHOW_DATE or DateUtils.FORMAT_SHOW_TIME or DateUtils.FORMAT_ABBREV_ALL,
        )
        add(stringResource(R.string.poll_closes_on, time))
      }
    }
  }
  Row(verticalAlignment = Alignment.CenterVertically) {
    Text(
      parts.joinToString(" · "),
      style = MaterialTheme.typography.labelMedium,
      color = colors.muted,
      modifier = Modifier.weight(1f),
    )
    if (poll.isMine && !poll.isClosed) {
      TextButton(onClick = onClose) { Text(stringResource(R.string.poll_close), color = colors.accent) }
    }
  }
}

/**
 * While the card is shown and its poll has a closing time ahead: one wait until then, after which
 * the core counts again (it takes the time as passed), so voting ends and held-back counts appear.
 */
@Composable
private fun RecountAtClosing(viewModel: MainViewModel, poll: Poll) {
  val closesAt = poll.maybeClosesAt ?: return
  if (poll.isClosed) return
  LaunchedEffect(closesAt) {
    val waitMs = closesAt - System.currentTimeMillis() + CLOSING_MARGIN_MS
    if (waitMs > MAX_CLOSING_WAIT_MS) return@LaunchedEffect
    delay(waitMs.coerceAtLeast(0))
    viewModel.noteTimePassed()
  }
}

/** "Choose one · Names hidden · After voting": how the poll is answered and what it shows. */
@Composable
private fun modeText(poll: Poll): String = buildList {
  add(stringResource(if (poll.isMultipleChoice) R.string.poll_multiple_choice else R.string.poll_single_choice))
  if (!poll.areVotersShown) add(stringResource(R.string.poll_hidden_voters))
  when (poll.results) {
    "always" -> {}
    "after_vote" -> add(stringResource(R.string.poll_results_after_vote))
    else -> add(stringResource(R.string.poll_results_after_close))
  }
}.joinToString(" · ")

/** The first names of who chose an option, then "and N more"; null when nobody did. */
@Composable
private fun votersLine(voters: List<String>, nameOf: (String) -> String): String? {
  if (voters.isEmpty()) return null
  val names = voters.take(MAX_LISTED_VOTERS).joinToString(", ") { nameOf(it) }
  val rest = voters.size - MAX_LISTED_VOTERS
  return if (rest > 0) "$names ${pluralStringResource(R.plurals.poll_voters_more, rest, rest)}" else names
}

/** The option with the most votes, when exactly one has the most and anybody voted. */
internal fun leaderOf(poll: Poll): Int? {
  val most = poll.options.maxOfOrNull { it.count }?.takeIf { it > 0 } ?: return null
  return poll.options.indices.singleOrNull { poll.options[it].count == most }
}
