package de.zeitgleich.teamcraft.ui

import de.zeitgleich.teamcraft.core.ChatActions
import de.zeitgleich.teamcraft.core.FileInfo
import de.zeitgleich.teamcraft.core.FileTransfer
import de.zeitgleich.teamcraft.core.PickedFile
import de.zeitgleich.teamcraft.core.SecuritySnapshot
import de.zeitgleich.teamcraft.core.SendOptions
import de.zeitgleich.teamcraft.core.VoiceMessagePolicy
import de.zeitgleich.teamcraft.core.WireJson
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.int
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.long
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** Voice messages (#204): speeds, the hold gesture, the policy and the wire shapes. */
class VoiceRulesTest {
  @Test
  fun speedsCycleAndAreRememberedByName() {
    assertEquals(VoiceSpeed.ONE_AND_HALF, VoiceSpeed.NORMAL.next())
    assertEquals(VoiceSpeed.NORMAL, VoiceSpeed.DOUBLE.next())
    assertEquals(VoiceSpeed.DOUBLE, VoiceSpeed.of("DOUBLE"))
    assertEquals(VoiceSpeed.NORMAL, VoiceSpeed.of(null))
    assertEquals(VoiceSpeed.NORMAL, VoiceSpeed.of("QUADRUPLE"))
  }

  @Test
  fun aHoldLocksWhenSlidUpAndCancelsWhenSlidToTheStart() {
    assertEquals(HoldGesture.RECORDING, VoiceGestures.of(-20f, -10f))
    assertEquals(HoldGesture.CANCEL, VoiceGestures.of(-VoiceGestures.CANCEL_DP, 0f))
    assertEquals(HoldGesture.LOCK, VoiceGestures.of(0f, -VoiceGestures.LOCK_DP))
    // Up wins over sideways: locking is the safe outcome.
    assertEquals(HoldGesture.LOCK, VoiceGestures.of(-200f, -200f))
    assertEquals(HoldGesture.RECORDING, VoiceGestures.of(200f, 50f))
  }

  @Test
  fun thePolicyDefaultsAndClampsItsLength() {
    val security = WireJson.decodeFromString(SecuritySnapshot.serializer(), """{"my_fingerprint":"ab"}""")
    assertEquals(VoiceMessagePolicy(enabled = true, maxMinutes = 10), security.voiceMessagePolicy)
    val off = WireJson.decodeFromString(SecuritySnapshot.serializer(), """{"voice_message_policy":{"enabled":false,"max_minutes":2}}""")
    assertFalse(off.voiceMessagePolicy.enabled)
    assertEquals(120, off.voiceMessagePolicy.maxSeconds)
    assertEquals(60, VoiceMessagePolicy(maxMinutes = 0).maxSeconds)
    assertEquals(3_600, VoiceMessagePolicy(maxMinutes = 500).maxSeconds)
  }

  @Test
  fun anAudioFileWithAWaveformIsAVoiceMessage() {
    val voice = WireJson.decodeFromString(
      FileInfo.serializer(),
      """{"conversation":"1","message":"2","file_id":"3","file_name":"voice.ogg","media_type":"audio/ogg","size_bytes":9,"transfer":{"state":"remote"},"maybe_duration_ms":4200,"maybe_waveform":[8,255,40]}""",
    )
    assertTrue(voice.isVoice)
    assertEquals(listOf(8, 255, 40), voice.maybeWaveform)
    assertFalse(voice.copy(maybeWaveform = null).isVoice)
    assertFalse(voice.copy(mediaType = "video/webm").isVoice)
    assertFalse(voice.copy(transfer = FileTransfer(state = "uploading")).isVoice)
  }

  @Test
  fun aSentVoiceMessageCarriesItsLengthAndWaveform() {
    val voice = PickedFile("voice-message-20261105-140312.ogg", "audio/ogg", "/staging/v.ogg", maybeDurationMs = 4200, maybeWaveform = listOf(8, 255))
    val json = Json.parseToJsonElement(ChatActions.send("7", "", SendOptions(files = listOf(voice)))).jsonObject
    val file = json["message"]!!.jsonObject["files"]!!.jsonArray[0].jsonObject
    assertEquals(4200L, file["duration_ms"]!!.jsonPrimitive.long)
    assertEquals(listOf(8, 255), file["waveform"]!!.jsonArray.map { it.jsonPrimitive.int })
    val plain = Json.parseToJsonElement(ChatActions.send("7", "", SendOptions(files = listOf(PickedFile("a.png", "image/png", "/a.png"))))).jsonObject
    val plainFile = plain["message"]!!.jsonObject["files"]!!.jsonArray[0].jsonObject
    assertNull(plainFile["duration_ms"])
    assertNull(plainFile["waveform"])
  }
}
