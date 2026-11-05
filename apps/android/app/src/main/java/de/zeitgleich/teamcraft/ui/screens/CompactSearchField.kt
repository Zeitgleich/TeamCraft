package de.zeitgleich.teamcraft.ui.screens

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Search
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.ui.theme.Spacing

private val FIELD_HEIGHT = 44.dp

/**
 * A filled, rounded one-line field that filters a list (Material 3's search bar shape at list
 * density): 44 dp instead of an outlined text field's 56, with no outline competing with the rows.
 * The [placeholder] names the field for screen readers too.
 */
@Composable
internal fun CompactSearchField(value: String, onChange: (String) -> Unit, placeholder: String, modifier: Modifier = Modifier) {
  val colors = MaterialTheme.colorScheme
  Surface(shape = CircleShape, color = colors.surfaceContainerHigh, modifier = modifier.fillMaxWidth()) {
    BasicTextField(
      value = value,
      onValueChange = onChange,
      singleLine = true,
      textStyle = MaterialTheme.typography.bodyLarge.copy(color = colors.onSurface),
      cursorBrush = SolidColor(colors.primary),
      modifier = Modifier.fillMaxWidth().heightIn(min = FIELD_HEIGHT).semantics { contentDescription = placeholder },
      decorationBox = { field ->
        Row(Modifier.padding(start = Spacing.m), verticalAlignment = Alignment.CenterVertically) {
          Icon(Icons.Filled.Search, contentDescription = null, tint = colors.onSurfaceVariant, modifier = Modifier.size(20.dp))
          Box(Modifier.weight(1f).padding(horizontal = Spacing.s), contentAlignment = Alignment.CenterStart) {
            if (value.isEmpty()) {
              Text(
                placeholder,
                style = MaterialTheme.typography.bodyLarge,
                color = colors.onSurfaceVariant,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
              )
            }
            field()
          }
          if (value.isNotEmpty()) {
            IconButton(onClick = { onChange("") }) {
              Icon(Icons.Filled.Close, contentDescription = stringResource(R.string.clear))
            }
          } else {
            Box(Modifier.size(Spacing.m))
          }
        }
      },
    )
  }
}
