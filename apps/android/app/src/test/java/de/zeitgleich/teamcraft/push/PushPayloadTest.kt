package de.zeitgleich.teamcraft.push

import java.util.Base64
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertNull
import org.junit.Test

class PushPayloadTest {
  @Test
  fun decodesTheSealedBytesWithOrWithoutPadding() {
    val sealed = byteArrayOf(1, -2, 3, 4, 5)
    val unpadded = Base64.getUrlEncoder().withoutPadding().encodeToString(sealed)
    val padded = Base64.getUrlEncoder().encodeToString(sealed)
    assertArrayEquals(sealed, PushPayload.fromFcmData(mapOf(PushPayload.FCM_DATA_KEY to unpadded)))
    assertArrayEquals(sealed, PushPayload.fromFcmData(mapOf(PushPayload.FCM_DATA_KEY to padded)))
  }

  @Test
  fun rejectsMissingEmptyAndMalformedPayloads() {
    assertNull(PushPayload.fromFcmData(emptyMap()))
    assertNull(PushPayload.fromFcmData(mapOf(PushPayload.FCM_DATA_KEY to "")))
    assertNull(PushPayload.fromFcmData(mapOf(PushPayload.FCM_DATA_KEY to "not base64 !!")))
  }
}
