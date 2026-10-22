package de.zeitgleich.teamcraft.ui.screens

import android.graphics.Bitmap
import androidx.compose.foundation.Image
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.FilterQuality
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.res.stringResource
import com.google.zxing.BarcodeFormat
import com.google.zxing.EncodeHintType
import com.google.zxing.WriterException
import com.google.zxing.qrcode.QRCodeWriter
import com.google.zxing.qrcode.decoder.ErrorCorrectionLevel
import de.zeitgleich.teamcraft.R

/** Quiet zone around the code in modules; the QR specification asks for four. */
private const val QUIET_ZONE_MODULES = 4
private const val DARK_PIXEL = 0xFF000000.toInt()
private const val LIGHT_PIXEL = 0xFFFFFFFF.toInt()

/**
 * Renders [text] as a QR code (black on white, error correction M). Renders nothing if the text
 * doesn't fit into a QR code; callers always show the text as well.
 */
@Composable
fun QrCode(text: String, modifier: Modifier = Modifier) {
  val maybeBitmap = remember(text) { encodeQrCode(text) } ?: return
  Image(
    bitmap = maybeBitmap,
    contentDescription = stringResource(R.string.link_qr_description),
    // One pixel per module: scale up without smoothing so the modules stay sharp.
    filterQuality = FilterQuality.None,
    contentScale = ContentScale.Fit,
    modifier = modifier.aspectRatio(1f),
  )
}

/** Encodes at one pixel per module; the composable scales the bitmap. */
private fun encodeQrCode(text: String): ImageBitmap? {
  val hints = mapOf(
    EncodeHintType.ERROR_CORRECTION to ErrorCorrectionLevel.M,
    EncodeHintType.MARGIN to QUIET_ZONE_MODULES,
    EncodeHintType.CHARACTER_SET to Charsets.UTF_8.name(),
  )
  val matrix = try {
    QRCodeWriter().encode(text, BarcodeFormat.QR_CODE, 0, 0, hints)
  } catch (error: WriterException) {
    // Too long for a QR code: the text fallback next to it still works.
    return null
  }
  val width = matrix.width
  val height = matrix.height
  val pixels = IntArray(width * height) { index ->
    if (matrix.get(index % width, index / width)) DARK_PIXEL else LIGHT_PIXEL
  }
  val bitmap = Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888)
  bitmap.setPixels(pixels, 0, width, 0, 0, width, height)
  return bitmap.asImageBitmap()
}
