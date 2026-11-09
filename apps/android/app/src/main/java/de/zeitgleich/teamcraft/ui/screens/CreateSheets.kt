package de.zeitgleich.teamcraft.ui.screens

import androidx.annotation.DrawableRes
import androidx.annotation.StringRes
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Edit
import androidx.compose.material.icons.filled.Person
import androidx.compose.material.icons.filled.Search
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Text
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.ChatClient
import de.zeitgleich.teamcraft.ui.MainViewModel
import de.zeitgleich.teamcraft.ui.forms.CreateField
import de.zeitgleich.teamcraft.ui.forms.CreateResult
import de.zeitgleich.teamcraft.ui.forms.FieldErrors
import de.zeitgleich.teamcraft.ui.forms.createErrors
import de.zeitgleich.teamcraft.ui.forms.fieldError
import de.zeitgleich.teamcraft.ui.forms.focusFirst
import de.zeitgleich.teamcraft.ui.forms.supportingText
import de.zeitgleich.teamcraft.ui.theme.Spacing
import kotlinx.coroutines.launch
import uniffi.teamcraft_ffi.CreateOutcome
import uniffi.teamcraft_ffi.NewChannelInput
import uniffi.teamcraft_ffi.NewSpaceInput
import uniffi.teamcraft_ffi.newRequestKey

/*
 * Starting things from the phone (#1534): the "New" menu behind Home's button, and the forms for a
 * new channel and a new space. A form closes only once the server took it, then the new channel or
 * space opens; until then it keeps the input, with every error at its field (forms.md).
 */

/** What the "New" menu offers; an entry the user may not use isn't there. */
internal class CreateChoices(
  /** The space a new channel goes into: Home's selected one, where the user may create channels. */
  val maybeChannelSpaceName: String?,
  val canCreateSpace: Boolean,
  /** Guests browse no channels, as on desktop. */
  val canBrowse: Boolean,
)

@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun CreateMenuSheet(
  choices: CreateChoices,
  onNewMessage: () -> Unit,
  onNewChannel: () -> Unit,
  onNewSpace: () -> Unit,
  onBrowse: () -> Unit,
  onDirectory: () -> Unit,
  onDismiss: () -> Unit,
) {
  ModalBottomSheet(onDismissRequest = onDismiss, sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)) {
    Column(Modifier.fillMaxWidth().navigationBarsPadding().padding(bottom = Spacing.l)) {
      SheetTitle(stringResource(R.string.create_new))
      CreateEntry(Icons.Filled.Edit, R.string.dm_new, stringResource(R.string.create_message_hint), onNewMessage)
      choices.maybeChannelSpaceName?.let { space ->
        CreateEntry(R.drawable.ic_tag, R.string.channel_create, stringResource(R.string.create_channel_in, space), onNewChannel)
      }
      if (choices.canCreateSpace) {
        CreateEntry(R.drawable.ic_workspaces, R.string.create_space, stringResource(R.string.create_space_hint), onNewSpace)
      }
      if (choices.canBrowse) {
        CreateEntry(Icons.Filled.Search, R.string.browse_channels, stringResource(R.string.create_browse_hint), onBrowse)
      }
      // Everybody has a directory; a guest's lists the people they share a conversation with.
      CreateEntry(Icons.Filled.Person, R.string.directory_title, stringResource(R.string.directory_hint), onDirectory)
    }
  }
}

/** "New channel" in [spaceId]: name, who can join, description. Answers the new channel's ID. */
@Composable
internal fun NewChannelSheet(
  viewModel: MainViewModel,
  spaceId: String,
  spaceName: String,
  onCreated: (String) -> Unit,
  onDismiss: () -> Unit,
) {
  var isPrivate by rememberSaveable { mutableStateOf(false) }
  CreateFormSheet(
    viewModel = viewModel,
    title = stringResource(R.string.create_channel_title, spaceName),
    nameLabel = R.string.channel_name_label,
    whenNameEmpty = R.string.channel_name_missing,
    submit = R.string.create_channel_submit,
    access = AccessChoice(
      label = R.string.channel_access_label,
      isFirst = !isPrivate,
      first = R.string.channel_access_public to R.string.channel_access_public_hint,
      second = R.string.channel_access_private to R.string.channel_access_private_hint,
      onPick = { isFirst -> isPrivate = !isFirst },
    ),
    create = { name, description, key ->
      createChannel(NewChannelInput(spaceId, name.trim(), isPrivate, description, key))
    },
    onCreated = onCreated,
    onDismiss = onDismiss,
  )
}

/** "New space": name, who can join, description. Answers the new space's ID. */
@Composable
internal fun NewSpaceSheet(viewModel: MainViewModel, onCreated: (String) -> Unit, onDismiss: () -> Unit) {
  var isOpen by rememberSaveable { mutableStateOf(true) }
  CreateFormSheet(
    viewModel = viewModel,
    title = stringResource(R.string.create_space),
    nameLabel = R.string.space_name_label,
    whenNameEmpty = R.string.space_name_missing,
    submit = R.string.create_space_submit,
    access = AccessChoice(
      label = R.string.channel_access_label,
      isFirst = isOpen,
      first = R.string.space_access_open to R.string.space_access_open_hint,
      second = R.string.space_access_invite to R.string.space_access_invite_hint,
      onPick = { isOpen = it },
    ),
    create = { name, description, key -> createSpace(NewSpaceInput(name.trim(), description, isOpen, key)) },
    onCreated = onCreated,
    onDismiss = onDismiss,
  )
}

/** Two choices of who can join, each with a line that says what it means. */
private class AccessChoice(
  @StringRes val label: Int,
  val isFirst: Boolean,
  val first: Pair<Int, Int>,
  val second: Pair<Int, Int>,
  val onPick: (Boolean) -> Unit,
)

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun CreateFormSheet(
  viewModel: MainViewModel,
  title: String,
  @StringRes nameLabel: Int,
  @StringRes whenNameEmpty: Int,
  @StringRes submit: Int,
  access: AccessChoice,
  create: suspend ChatClient.(String, String, String) -> CreateOutcome,
  onCreated: (String) -> Unit,
  onDismiss: () -> Unit,
) {
  val scope = rememberCoroutineScope()
  var name by rememberSaveable { mutableStateOf("") }
  var description by rememberSaveable { mutableStateOf("") }
  // One key per attempt: a retry after a lost answer creates nothing twice; a refused request
  // takes a new one, since the input changes.
  var requestKey by rememberSaveable { mutableStateOf(newRequestKey()) }
  var hasTried by rememberSaveable { mutableStateOf(false) }
  var isCreating by remember { mutableStateOf(false) }
  var maybeNameRefusal by remember { mutableStateOf<Int?>(null) }
  var maybeFailure by remember { mutableStateOf<Int?>(null) }
  val requesters = remember { CreateField.entries.associateWith { FocusRequester() } }
  val checked = createErrors(name, description, whenNameEmpty)
  val shown = (if (hasTried) checked else FieldErrors.of<CreateField>())
    .withServerError(CreateField.NAME, maybeNameRefusal)
  val send: () -> Unit = send@{
    hasTried = true
    if (!checked.isValid) {
      checked.focusFirst(requesters)
      return@send
    }
    if (isCreating) return@send
    isCreating = true
    maybeFailure = null
    scope.launch {
      when (val result = viewModel.tryCreate { create(name, description.trim(), requestKey) }) {
        is CreateResult.Created -> onCreated(result.id)
        CreateResult.NameRefused -> {
          maybeNameRefusal = R.string.create_name_refused
          requestKey = newRequestKey()
          requesters[CreateField.NAME]?.requestFocus()
        }
        is CreateResult.Failed -> {
          maybeFailure = result.text
          if (result.text == R.string.create_not_allowed) requestKey = newRequestKey()
        }
      }
      isCreating = false
    }
  }
  // The keyboard comes up with the sheet: naming it is the first step.
  LaunchedEffect(Unit) { requesters.getValue(CreateField.NAME).requestFocus() }
  ModalBottomSheet(
    // While the server is asked the sheet stays, so its answer has somewhere to show.
    onDismissRequest = { if (!isCreating) onDismiss() },
    sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
  ) {
    Column(
      Modifier
        .fillMaxWidth()
        .imePadding()
        .navigationBarsPadding()
        .verticalScroll(rememberScrollState())
        .padding(horizontal = Spacing.l)
        .padding(bottom = Spacing.l),
      verticalArrangement = Arrangement.spacedBy(Spacing.m),
    ) {
      Text(title, style = MaterialTheme.typography.titleLarge, modifier = Modifier.semantics { heading() })
      OutlinedTextField(
        value = name,
        onValueChange = {
          name = it
          maybeNameRefusal = null
        },
        label = { Text(stringResource(nameLabel)) },
        singleLine = true,
        isError = shown[CreateField.NAME] != null,
        supportingText = supportingText(shown[CreateField.NAME]),
        keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.Sentences, imeAction = ImeAction.Next),
        modifier = Modifier
          .fillMaxWidth()
          .focusRequester(requesters.getValue(CreateField.NAME))
          .fieldError(shown[CreateField.NAME]),
      )
      AccessGroup(access)
      OutlinedTextField(
        value = description,
        onValueChange = { description = it },
        label = { Text(stringResource(R.string.create_description_label)) },
        minLines = 2,
        isError = shown[CreateField.DESCRIPTION] != null,
        supportingText = supportingText(shown[CreateField.DESCRIPTION]),
        keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.Sentences),
        modifier = Modifier
          .fillMaxWidth()
          .focusRequester(requesters.getValue(CreateField.DESCRIPTION))
          .fieldError(shown[CreateField.DESCRIPTION]),
      )
      // Failures that belong to no field sit next to the button (forms.md).
      maybeFailure?.let { failure ->
        Text(
          stringResource(failure),
          color = MaterialTheme.colorScheme.error,
          style = MaterialTheme.typography.bodyMedium,
          modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
        )
      }
      Button(onClick = send, enabled = !isCreating, modifier = Modifier.fillMaxWidth()) {
        if (isCreating) {
          CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp, color = MaterialTheme.colorScheme.onPrimary)
        } else {
          Text(stringResource(submit))
        }
      }
    }
  }
}

@Composable
private fun AccessGroup(access: AccessChoice) {
  Column(Modifier.fillMaxWidth().selectableGroup()) {
    Text(
      stringResource(access.label),
      style = MaterialTheme.typography.labelLarge,
      color = MaterialTheme.colorScheme.onSurfaceVariant,
      modifier = Modifier.padding(bottom = Spacing.xs),
    )
    AccessOption(access.first, isSelected = access.isFirst) { access.onPick(true) }
    AccessOption(access.second, isSelected = !access.isFirst) { access.onPick(false) }
  }
}

@Composable
private fun AccessOption(texts: Pair<Int, Int>, isSelected: Boolean, onSelect: () -> Unit) {
  Row(
    Modifier
      .fillMaxWidth()
      .clip(RoundedCornerShape(12.dp))
      .selectable(selected = isSelected, role = Role.RadioButton, onClick = onSelect)
      .padding(vertical = Spacing.xs),
    verticalAlignment = Alignment.CenterVertically,
  ) {
    RadioButton(selected = isSelected, onClick = null, modifier = Modifier.padding(horizontal = Spacing.s))
    Column(Modifier.weight(1f)) {
      Text(stringResource(texts.first), style = MaterialTheme.typography.bodyLarge)
      Text(stringResource(texts.second), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
    }
  }
}

@Composable
private fun SheetTitle(text: String) {
  Text(
    text,
    style = MaterialTheme.typography.titleMedium,
    modifier = Modifier.padding(horizontal = Spacing.l, vertical = Spacing.s).semantics { heading() },
  )
}

@Composable
private fun CreateEntry(icon: ImageVector, @StringRes title: Int, hint: String, onClick: () -> Unit) =
  CreateEntryRow(title, hint, onClick) { Icon(icon, contentDescription = null, tint = MaterialTheme.colorScheme.primary) }

@Composable
private fun CreateEntry(@DrawableRes icon: Int, @StringRes title: Int, hint: String, onClick: () -> Unit) =
  CreateEntryRow(title, hint, onClick) {
    Icon(painterResource(icon), contentDescription = null, tint = MaterialTheme.colorScheme.primary)
  }

@Composable
private fun CreateEntryRow(@StringRes title: Int, hint: String, onClick: () -> Unit, icon: @Composable () -> Unit) {
  ListItem(
    headlineContent = { Text(stringResource(title)) },
    supportingContent = { Text(hint) },
    leadingContent = icon,
    colors = ListItemDefaults.colors(containerColor = MaterialTheme.colorScheme.surfaceContainerLow),
    modifier = Modifier.clickable(role = Role.Button, onClick = onClick),
  )
}
