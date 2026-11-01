package de.zeitgleich.teamcraft.ui.forms

import androidx.annotation.StringRes
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.error
import androidx.compose.ui.semantics.semantics

/*
 * forms.md on Android: the error is the field's `isError` and `supportingText`, directly below it,
 * and TalkBack reads it as the field's error; a refused submit focuses the first invalid field.
 */

/**
 * The field's `supportingText`: its error when it has one, else [maybeHint] (a rule such as the
 * minimum length), else nothing.
 */
fun supportingText(@StringRes maybeError: Int?, @StringRes maybeHint: Int? = null): (@Composable () -> Unit)? {
  val text = maybeError ?: maybeHint ?: return null
  return { Text(stringResource(text)) }
}

/** Names the field's error for TalkBack, in the words shown below it. */
@Composable
fun Modifier.fieldError(@StringRes maybeError: Int?): Modifier {
  val message = maybeError?.let { stringResource(it) } ?: return this
  return semantics { error(message) }
}

/**
 * After a refused submit: the first invalid field takes the focus, and the scroller brings it into
 * view. [requesters] maps each field to its field's [FocusRequester].
 */
fun <F : Enum<F>> FieldErrors<F>.focusFirst(requesters: Map<F, FocusRequester>) {
  maybeFirst?.let { requesters[it]?.requestFocus() }
}
