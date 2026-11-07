package de.zeitgleich.teamcraft.ui.screens

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class DraftSaveTest {
  @Test
  fun leavingWithinTheTypingPauseStoresTheText() {
    // #1220: typed, then another conversation opened before the pause was over.
    val save = DraftSave().apply { stored("") }
    val edit = save.edit("half a thought")
    assertEquals("half a thought", save.takePending())
    assertNull("the pause's wait writes nothing more", save.takeDue(edit))
  }

  @Test
  fun nothingIsWrittenWhenNothingChanged() {
    val save = DraftSave()
    assertNull("never typed", save.takePending())
    save.stored("from another device")
    assertNull("the loaded draft is not written back", save.takeDue(save.edit("from another device")))
    val edit = save.edit("from another device, edited")
    assertEquals("from another device, edited", save.takeDue(edit))
    assertNull("already stored: leaving writes nothing", save.takePending())
  }

  @Test
  fun onlyTheLastEditsPauseStores() {
    val save = DraftSave()
    val first = save.edit("a")
    val second = save.edit("ab")
    assertNull("a later edit restarted the pause", save.takeDue(first))
    assertEquals("ab", save.takeDue(second))
  }
}
