package de.zeitgleich.teamcraft.calls

import de.zeitgleich.teamcraft.R
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
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

  private fun hint(
    phase: CallPhase = CallPhase.CONNECTED,
    isMuted: Boolean = false,
    isVideoHeldForVoice: Boolean = false,
    isOnLimitedNetwork: Boolean = false,
  ): Int? = statusHintRes(phase, isMuted, isVideoHeldForVoice, isOnLimitedNetwork)
}
