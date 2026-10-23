package de.zeitgleich.teamcraft.ui.screens

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Lock
import androidx.compose.material3.Button
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R

/** Covers the app while it is locked; asks for the unlock right away and again on the button. */
@Composable
fun LockScreen(onUnlock: () -> Unit) {
  LaunchedEffect(Unit) { onUnlock() }
  Surface(Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.background) {
    Column(
      Modifier.fillMaxSize().padding(32.dp),
      verticalArrangement = Arrangement.spacedBy(16.dp, Alignment.CenterVertically),
      horizontalAlignment = Alignment.CenterHorizontally,
    ) {
      Icon(Icons.Filled.Lock, contentDescription = null, tint = MaterialTheme.colorScheme.primary)
      Text(stringResource(R.string.app_lock_locked), style = MaterialTheme.typography.titleMedium)
      Button(onClick = onUnlock) { Text(stringResource(R.string.app_lock_unlock)) }
    }
  }
}
