package de.zeitgleich.teamcraft.ui.screens

import android.text.format.DateUtils
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.Button
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.Meeting
import de.zeitgleich.teamcraft.ui.MainViewModel

/** Upcoming and running meetings; joining one joins its conversation's call. */
@Composable
fun MeetingsScreen(viewModel: MainViewModel, onBack: () -> Unit) {
  val changes by viewModel.changeCount.collectAsState()
  val context = LocalContext.current
  var meetings by remember { mutableStateOf<List<Meeting>?>(null) }
  LaunchedEffect(changes) { meetings = viewModel.readChat { meetings() } }
  Scaffold(topBar = { ChromeTopBar(title = stringResource(R.string.meetings_title), maybeOnBack = onBack) }, contentWindowInsets = ScreenInsets) { padding ->
    LazyColumn(Modifier.fillMaxSize().padding(padding)) {
      val list = meetings.orEmpty().filter { it.status != "cancelled" && it.status != "ended" }.sortedBy { it.startsAt }
      if (meetings != null && list.isEmpty()) item { EmptyText(R.string.meetings_empty) }
      // A repeating meeting is listed once per occurrence, all with the series' ID.
      items(list, key = { "${it.id}-${it.startsAt}" }) { meeting ->
        Row(Modifier.fillMaxWidth().padding(16.dp), verticalAlignment = Alignment.CenterVertically) {
          Column(Modifier.weight(1f)) {
            Text(meeting.title, style = MaterialTheme.typography.titleMedium)
            Text(
              DateUtils.formatDateRange(
                context,
                meeting.startsAt,
                meeting.endsAt,
                DateUtils.FORMAT_SHOW_DATE or DateUtils.FORMAT_SHOW_TIME,
              ),
              style = MaterialTheme.typography.bodySmall,
            )
          }
          Button(onClick = { viewModel.startCall(meeting.conversationId, meeting.title) }) {
            Text(stringResource(R.string.meeting_join))
          }
        }
        HorizontalDivider()
      }
    }
  }
}
