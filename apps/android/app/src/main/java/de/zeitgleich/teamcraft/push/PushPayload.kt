package de.zeitgleich.teamcraft.push

import java.util.Base64

/**
 * How the sealed wake-up (e2ee.md §7) travels per service. UnifiedPush delivers the raw bytes as
 * the message body; FCM data messages carry only strings, so the server puts the bytes base64url
 * encoded (no padding) under [FCM_DATA_KEY].
 */
object PushPayload {
  const val FCM_DATA_KEY = "w"

  /** The sealed bytes of an FCM data message, or `null` if it carries none or is malformed. */
  fun fromFcmData(data: Map<String, String>): ByteArray? {
    val encoded = data[FCM_DATA_KEY] ?: return null
    return try {
      Base64.getUrlDecoder().decode(encoded.trimEnd('='))
    } catch (malformed: IllegalArgumentException) {
      null
    }.takeIf { it?.isNotEmpty() == true }
  }
}
