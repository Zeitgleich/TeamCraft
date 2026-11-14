package de.zeitgleich.teamcraft.ui.forms

import de.zeitgleich.teamcraft.R
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.teamcraft_ffi.ChannelNameProblem

class CreateFormsTest {
  @Test
  fun aGoodNameAndNoDescriptionPass() {
    assertTrue(createErrors("Project A", "", R.string.channel_name_missing).isValid)
  }

  @Test
  fun anEmptyNameSaysWhatToEnter() {
    val errors = createErrors("   ", "", R.string.space_name_missing)
    assertEquals(R.string.space_name_missing, errors[CreateField.NAME])
    assertEquals(CreateField.NAME, errors.maybeFirst)
  }

  @Test
  fun everyErrorShowsAtOnceInFieldOrder() {
    val errors = createErrors("x".repeat(MAX_NAME_CHARS + 1), "y".repeat(MAX_DESCRIPTION_CHARS + 1), R.string.channel_name_missing)
    assertEquals(R.string.create_name_too_long, errors[CreateField.NAME])
    assertEquals(R.string.create_description_too_long, errors[CreateField.DESCRIPTION])
    assertEquals(CreateField.NAME, errors.maybeFirst)
  }

  @Test
  fun controlCharactersAreRefusedLikeTheServerDoes() {
    assertEquals(R.string.create_name_invalid, createErrors("a\u0007b", "", R.string.channel_name_missing)[CreateField.NAME])
  }

  @Test
  fun aChannelNameShowsTheProblemTheCoreFound() {
    val errors = createErrors("a\u200Bb", "", R.string.channel_name_missing, ChannelNameProblem.INVISIBLE_CHARACTER)
    assertEquals(R.string.channel_name_error_invisible, errors[CreateField.NAME])
    assertEquals(
      R.string.channel_name_error_control,
      createErrors("a\u0007b", "", R.string.channel_name_missing, ChannelNameProblem.CONTROL_CHARACTER)[CreateField.NAME],
    )
  }

  @Test
  fun anEmptyChannelNameStillSaysWhatToEnter() {
    val errors = createErrors("  ", "", R.string.channel_name_missing, ChannelNameProblem.EMPTY)
    assertEquals(R.string.channel_name_missing, errors[CreateField.NAME])
  }

  @Test
  fun theLimitsCountTrimmedText() {
    assertNull(createErrors(" " + "x".repeat(MAX_NAME_CHARS) + " ", "", R.string.channel_name_missing)[CreateField.NAME])
  }
}
