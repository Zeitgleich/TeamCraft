package de.zeitgleich.teamcraft.calls

import de.zeitgleich.teamcraft.R
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.teamcraft_ffi.CallPhase

class CallStatusHintTest {
  @Test
  fun aHeldOwnVideoSaysTheVoiceComesFirst() {
    assertEquals(R.string.call_video_held_for_voice, hint(isVideoHeldForVoice = true))
  }

  @Test
  fun theVoiceFirstNoteGoesBeforeTheLimitedNetwork() {
    assertEquals(R.string.call_video_held_for_voice, hint(isVideoHeldForVoice = true, isOnLimitedNetwork = true))
    assertEquals(R.string.call_limited_network, hint(isOnLimitedNetwork = true))
  }

  @Test
  fun othersVideoYieldingToTheOwnVoiceSaysSo() {
    assertEquals(R.string.call_video_yielded_for_voice, hint(isVideoYieldedForVoice = true))
    assertEquals(R.string.call_video_yielded_for_voice, hint(isVideoYieldedForVoice = true, isOnLimitedNetwork = true))
    assertEquals(R.string.call_video_held_for_voice, hint(isVideoHeldForVoice = true, isVideoYieldedForVoice = true))
  }

  @Test
  fun reconnectingAndMutedStayFirst() {
    assertEquals(R.string.call_reconnecting, hint(phase = CallPhase.RECONNECTING, isVideoHeldForVoice = true))
    assertEquals(R.string.call_you_muted, hint(isMuted = true, isVideoHeldForVoice = true))
  }

  @Test
  fun noHintBeforeTheCallIsConnectedOrWithoutCause() {
    assertNull(hint(phase = CallPhase.CONNECTING, isVideoHeldForVoice = true))
    assertNull(hint(phase = CallPhase.ENDED, isVideoHeldForVoice = true))
    assertNull(hint())
  }

  @Test
  fun theSelfViewSaysSoOnlyWhileConnected() {
    assertTrue(isSelfViewHeldForVoice(CallPhase.CONNECTED, isVideoHeldForVoice = true))
    assertFalse(isSelfViewHeldForVoice(CallPhase.RECONNECTING, isVideoHeldForVoice = true))
    assertFalse(isSelfViewHeldForVoice(null, isVideoHeldForVoice = true))
    assertFalse(isSelfViewHeldForVoice(CallPhase.CONNECTED, isVideoHeldForVoice = false))
  }

  private fun hint(
    phase: CallPhase = CallPhase.CONNECTED,
    isMuted: Boolean = false,
    isVideoHeldForVoice: Boolean = false,
    isVideoYieldedForVoice: Boolean = false,
    isOnLimitedNetwork: Boolean = false,
  ): Int? = statusHintRes(phase, isMuted, isVideoHeldForVoice, isVideoYieldedForVoice, isOnLimitedNetwork)
}
