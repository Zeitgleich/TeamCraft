package de.zeitgleich.teamcraft.core

import de.zeitgleich.teamcraft.R
import org.junit.Assert.assertEquals
import org.junit.Test
import uniffi.teamcraft_ffi.CommandFailure
import uniffi.teamcraft_ffi.CommandFailureKind

class CommandFailureTextTest {
  @Test
  fun aRefusedDeletionIsWordedByTheServersCode() {
    assertEquals(R.string.error_legal_hold, refused("legal_hold"))
    assertEquals(R.string.error_last_channel_admin, refused("last_channel_admin"))
    assertEquals(R.string.error_import_in_progress, refused("import_in_progress"))
    assertEquals(R.string.error_space_has_hidden_channels, refused("space_has_hidden_channels"))
  }

  @Test
  fun everyNotFoundCodeSaysItIsGone() {
    assertEquals(R.string.error_not_found, refused("conversation_not_found"))
    assertEquals(R.string.error_not_found, refused("some_future_thing_not_found"))
  }

  @Test
  fun unknownCodesAreGeneric() {
    assertEquals(R.string.error_generic, refused("brand_new_code"))
    assertEquals(R.string.error_generic, commandFailureText(CommandFailure(CommandFailureKind.REFUSED, null, "x")))
  }

  @Test
  fun unreachableAndOtherHaveTheirOwnTexts() {
    assertEquals(
      R.string.error_network,
      commandFailureText(CommandFailure(CommandFailureKind.UNREACHABLE, null, "offline")),
    )
    assertEquals(R.string.error_generic, commandFailureText(CommandFailure(CommandFailureKind.OTHER, null, "store")))
  }

  private fun refused(code: String) = commandFailureText(CommandFailure(CommandFailureKind.REFUSED, code, "refused"))
}
