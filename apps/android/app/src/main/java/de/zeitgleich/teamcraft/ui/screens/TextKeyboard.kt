package de.zeitgleich.teamcraft.ui.screens

import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.runtime.Composable
import androidx.compose.runtime.ReadOnlyComposable
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.intl.LocaleList

/**
 * Keyboard options of every free-text field (composer, edit, search, create sheets).
 *
 * Compose passes no `EditorInfo.hintLocales` by default, so a keyboard with several languages
 * enabled guesses one (glide typing then offered English words on a German phone). The hint is
 * the app's current locales, which follow the system or the per-app language.
 */
@Composable
@ReadOnlyComposable
fun textKeyboard(
  capitalization: KeyboardCapitalization = KeyboardCapitalization.Sentences,
  imeAction: ImeAction = ImeAction.Default,
): KeyboardOptions = KeyboardOptions(
  capitalization = capitalization,
  imeAction = imeAction,
  hintLocales = LocaleList.current,
)
