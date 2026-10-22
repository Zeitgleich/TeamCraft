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
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.delay
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.ui.MainViewModel
import uniffi.teamcraft_ffi.SearchHitItem

private const val TYPING_PAUSE_MS = 250L

@Composable
fun SearchScreen(viewModel: MainViewModel, onOpen: (String) -> Unit, onBack: () -> Unit) {
  var query by rememberSaveable { mutableStateOf("") }
  var hits by remember { mutableStateOf<List<SearchHitItem>>(emptyList()) }
  LaunchedEffect(query) {
    delay(TYPING_PAUSE_MS)
    hits = viewModel.search(query)
  }
  Scaffold(topBar = { ChromeTopBar(title = stringResource(R.string.search), maybeOnBack = onBack) }) { padding ->
    Column(Modifier.fillMaxSize().padding(padding)) {
      OutlinedTextField(
        value = query,
        onValueChange = { query = it },
        placeholder = { Text(stringResource(R.string.search_hint)) },
        singleLine = true,
        modifier = Modifier.fillMaxWidth().padding(12.dp),
      )
      if (query.isNotBlank() && hits.isEmpty()) {
        Text(stringResource(R.string.search_empty), modifier = Modifier.padding(16.dp))
      }
      LazyColumn {
        items(hits, key = { it.message.id }) { hit ->
          Column(Modifier.fillMaxWidth().clickable { onOpen(hit.message.conversationId) }) {
            Text(
              listOfNotNull(hit.maybeSpaceName, hit.conversationName).joinToString(" › "),
              style = MaterialTheme.typography.labelMedium,
              color = MaterialTheme.colorScheme.primary,
              modifier = Modifier.padding(start = 16.dp, top = 8.dp),
            )
            Text(
              hit.message.authorName,
              style = MaterialTheme.typography.labelLarge,
              modifier = Modifier.padding(start = 16.dp, top = 2.dp),
            )
            Text(hit.message.body, modifier = Modifier.padding(start = 16.dp, end = 16.dp, bottom = 8.dp))
          }
        }
      }
    }
  }
}
