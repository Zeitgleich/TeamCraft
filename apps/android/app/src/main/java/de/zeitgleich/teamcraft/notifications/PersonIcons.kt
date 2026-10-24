package de.zeitgleich.teamcraft.notifications

import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Paint
import android.graphics.Typeface
import androidx.core.graphics.ColorUtils
import androidx.core.graphics.drawable.IconCompat

/**
 * The generated avatar of a notification's sender, as in the app: initials on the sender's hue.
 * Drawn per post (a few small bitmaps); no picture is fetched in the background.
 */
object PersonIcons {
  private const val SIZE_PX = 128
  private const val SATURATION = 0.45f
  private const val LIGHTNESS = 0.32f
  private const val TEXT_SCALE = 0.42f

  fun of(key: String, name: String): IconCompat = IconCompat.createWithBitmap(bitmap(key, name))

  fun bitmap(key: String, name: String): Bitmap {
    val bitmap = Bitmap.createBitmap(SIZE_PX, SIZE_PX, Bitmap.Config.ARGB_8888)
    val canvas = Canvas(bitmap)
    val background = Paint(Paint.ANTI_ALIAS_FLAG).apply {
      color = ColorUtils.HSLToColor(floatArrayOf(NotificationStyling.hue(key).toFloat(), SATURATION, LIGHTNESS))
    }
    val half = SIZE_PX / 2f
    canvas.drawCircle(half, half, half, background)
    val text = Paint(Paint.ANTI_ALIAS_FLAG).apply {
      color = android.graphics.Color.WHITE
      textSize = SIZE_PX * TEXT_SCALE
      textAlign = Paint.Align.CENTER
      typeface = Typeface.create(Typeface.DEFAULT, Typeface.BOLD)
    }
    val baseline = half - (text.descent() + text.ascent()) / 2
    canvas.drawText(NotificationStyling.initials(name), half, baseline, text)
    return bitmap
  }
}
