package de.zeitgleich.teamcraft.calls

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class AudioRoutesTest {
  @Test
  fun aPickerOnlyWhenThereIsMoreThanEarpieceAndSpeaker() {
    val earpiece = AudioRoute("1", AudioRouteKind.EARPIECE, "Earpiece")
    val speaker = AudioRoute("2", AudioRouteKind.SPEAKER, "Speaker")
    val headset = AudioRoute("3", AudioRouteKind.BLUETOOTH, "Headset")
    assertFalse(needsRoutePicker(listOf(earpiece, speaker)))
    assertTrue(needsRoutePicker(listOf(earpiece, speaker, headset)))
  }
}
