package de.zeitgleich.teamcraft.ui.screens

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.Conversation
import de.zeitgleich.teamcraft.ui.MainViewModel
import de.zeitgleich.teamcraft.ui.SharedContent

/**
 * Where to share what another app sent: the conversations the user can post in, filterable. The
 * pick opens the conversation with the composer filled in; nothing is sent without the user.
 */
@Composable
fun SharePickerScreen(viewModel: MainViewModel, content: SharedContent, onPick: (String) -> Unit, onBack: () -> Unit) {
  var conversations by remember { mutableStateOf<List<Conversation>?>(null) }
  var filter by rememberSaveable { mutableStateOf("") }
  LaunchedEffect(Unit) {
    conversations = viewModel.readChat { snapshot() }?.conversations?.filter { it.canPost }?.sortedBy { it.name.lowercase() }
  }
  Scaffold(
    contentWindowInsets = ScreenInsets,
    topBar = {
      ChromeTopBar(
        title = stringResource(R.string.share_title),
        maybeSubtitle = shareSummary(content),
        maybeOnBack = onBack,
      )
    },
  ) { padding ->
    Column(Modifier.fillMaxSize().padding(padding)) {
      OutlinedTextField(
        value = filter,
        onValueChange = { filter = it },
        placeholder = { Text(stringResource(R.string.share_filter)) },
        singleLine = true,
        modifier = Modifier.fillMaxWidth().padding(12.dp),
      )
      val loaded = conversations
      when {
        loaded == null -> LoadingScreen()
        else -> LazyColumn(Modifier.fillMaxSize()) {
          items(loaded.filter { it.name.contains(filter.trim(), ignoreCase = true) }, key = { it.id }) { conversation ->
            Text(
              conversation.name,
              style = MaterialTheme.typography.bodyLarge,
              maxLines = 1,
              overflow = TextOverflow.Ellipsis,
              modifier = Modifier.fillMaxWidth().clickable { onPick(conversation.id) }.padding(horizontal = 16.dp, vertical = 14.dp),
            )
          }
        }
      }
    }
  }
}

@Composable
private fun shareSummary(content: SharedContent): String = when {
  content.uris.isNotEmpty() -> pluralStringResource(R.plurals.share_files, content.uris.size, content.uris.size)
  else -> content.text
}
