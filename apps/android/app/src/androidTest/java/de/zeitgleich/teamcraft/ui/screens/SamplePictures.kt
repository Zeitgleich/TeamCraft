package de.zeitgleich.teamcraft.ui.screens

import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.LinearGradient
import android.graphics.Paint
import android.graphics.RectF
import android.graphics.Shader
import java.io.ByteArrayOutputStream

/** A drawn stand-in for a photo: sky, sun and hills, as a JPEG like senders make. */
fun samplePhoto(width: Int, height: Int): ByteArray {
  val bitmap = Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888)
  val canvas = Canvas(bitmap)
  val paint = Paint(Paint.ANTI_ALIAS_FLAG)
  paint.shader = LinearGradient(0f, 0f, 0f, height.toFloat(), 0xFF3B7DD8.toInt(), 0xFFF2C38B.toInt(), Shader.TileMode.CLAMP)
  canvas.drawRect(0f, 0f, width.toFloat(), height.toFloat(), paint)
  paint.shader = null
  paint.color = 0xFFFFE08A.toInt()
  canvas.drawCircle(width * 0.7f, height * 0.3f, width * 0.12f, paint)
  paint.color = 0xFF2E6B3A.toInt()
  canvas.drawCircle(width * 0.2f, height * 1.1f, width * 0.6f, paint)
  paint.color = 0xFF3F8A4C.toInt()
  canvas.drawCircle(width * 0.9f, height * 1.2f, width * 0.7f, paint)
  return ByteArrayOutputStream().also { bitmap.compress(Bitmap.CompressFormat.JPEG, 80, it) }.toByteArray()
}

/**
 * A drawn stand-in for a camera picture in a call: a soft room in [hue] with a person's outline
 * (head and shoulders), so call tiles show no real face.
 */
fun sampleCameraFrame(width: Int, height: Int, hue: Float): Bitmap {
  val bitmap = Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888)
  val canvas = Canvas(bitmap)
  val paint = Paint(Paint.ANTI_ALIAS_FLAG)
  val wall = android.graphics.Color.HSVToColor(floatArrayOf(hue, 0.35f, 0.55f))
  val floor = android.graphics.Color.HSVToColor(floatArrayOf(hue, 0.45f, 0.25f))
  paint.shader = LinearGradient(0f, 0f, 0f, height.toFloat(), wall, floor, Shader.TileMode.CLAMP)
  canvas.drawRect(0f, 0f, width.toFloat(), height.toFloat(), paint)
  paint.shader = null
  // A window's light on the wall behind.
  paint.color = 0x22FFFFFF
  canvas.drawRoundRect(RectF(width * 0.08f, height * 0.12f, width * 0.32f, height * 0.6f), 12f, 12f, paint)
  val skin = android.graphics.Color.HSVToColor(floatArrayOf(28f, 0.35f, 0.85f))
  val shirt = android.graphics.Color.HSVToColor(floatArrayOf((hue + 180f) % 360f, 0.45f, 0.7f))
  paint.color = shirt
  canvas.drawOval(RectF(width * 0.27f, height * 0.72f, width * 0.73f, height * 1.3f), paint)
  paint.color = skin
  canvas.drawCircle(width * 0.5f, height * 0.5f, height * 0.18f, paint)
  return bitmap
}
