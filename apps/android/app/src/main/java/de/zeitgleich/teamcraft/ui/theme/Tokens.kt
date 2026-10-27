package de.zeitgleich.teamcraft.ui.theme

import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Shapes
import androidx.compose.material3.Typography
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/**
 * Spacing steps shared by all screens, so gaps line up (a 4 dp grid, as the desktop's CSS uses
 * multiples of 4 px).
 */
object Spacing {
  val xs = 4.dp
  val s = 8.dp
  val m = 12.dp
  val l = 16.dp
  val xl = 24.dp
  val xxl = 32.dp
}

/**
 * Corner radii of crates/teamcraft-ui/style/main/000-tokens.css (`--radius-sm`, `--radius`), plus a
 * large one for cards.
 */
object Radius {
  val small = 5.dp
  val medium = 8.dp
  val large = 12.dp
}

internal val TeamCraftShapes = Shapes(
  extraSmall = RoundedCornerShape(Radius.small),
  small = RoundedCornerShape(Radius.small),
  medium = RoundedCornerShape(Radius.medium),
  large = RoundedCornerShape(Radius.large),
)

/**
 * One type scale: Material's sizes (sp, so they follow the system font scale) with the desktop's
 * weights: titles semi-bold instead of thin, labels medium.
 */
internal val TeamCraftTypography = Typography().let { base ->
  base.copy(
    titleLarge = base.titleLarge.semiBold(),
    titleMedium = base.titleMedium.semiBold(),
    titleSmall = base.titleSmall.copy(fontWeight = FontWeight.Medium),
    labelLarge = base.labelLarge.copy(fontWeight = FontWeight.Medium),
    labelMedium = base.labelMedium.copy(fontWeight = FontWeight.Medium, letterSpacing = 0.2.sp),
  )
}

private fun TextStyle.semiBold() = copy(fontWeight = FontWeight.SemiBold)
