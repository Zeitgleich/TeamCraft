package de.zeitgleich.teamcraft.ui

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class EmojiInputTest {
  @Test
  fun queryIsTheNameAfterAColonAtTheEnd() {
    assertEquals("thu", EmojiInput.activeQuery("great :thu"))
    assertEquals("+1", EmojiInput.activeQuery(":+1"))
    assertEquals("raised_ha", EmojiInput.activeQuery("line one\n:raised_ha"))
  }

  @Test
  fun emoticonsUrlsAndFinishedCodesAreNoQuery() {
    assertNull(EmojiInput.activeQuery("nice :)"))
    assertNull(EmojiInput.activeQuery("ok :D"))
    assertNull(EmojiInput.activeQuery("see https://example.com"))
    assertNull(EmojiInput.activeQuery("done :tada: "))
    assertNull(EmojiInput.activeQuery(":tada:"))
    assertNull(EmojiInput.activeQuery("time 10:30"))
  }

  @Test
  fun completingReplacesTheTypedName() {
    assertEquals("great 👍 ", EmojiInput.complete("great :thu", "👍"))
  }
}
