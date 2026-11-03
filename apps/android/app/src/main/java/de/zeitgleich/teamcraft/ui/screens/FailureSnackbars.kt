package de.zeitgleich.teamcraft.ui.screens

import androidx.compose.material3.SnackbarHostState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.platform.LocalContext
import de.zeitgleich.teamcraft.ui.MainViewModel

/**
 * The actions that failed ([MainViewModel.lastFailure]) as this screen's snackbars, so no failure
 * passes silently (issue #322). Each failure is taken once: with the list and a conversation side
 * by side (wide windows), only one of them shows it.
 */
@Composable
fun rememberFailureSnackbars(viewModel: MainViewModel): SnackbarHostState {
  val snackbars = remember { SnackbarHostState() }
  val maybeFailure by viewModel.lastFailure.collectAsState()
  val context = LocalContext.current
  LaunchedEffect(maybeFailure) {
    if (maybeFailure == null) return@LaunchedEffect
    viewModel.takeFailure()?.let { snackbars.showSnackbar(context.getString(it)) }
  }
  return snackbars
}
