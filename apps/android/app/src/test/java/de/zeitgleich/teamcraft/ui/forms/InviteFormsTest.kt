package de.zeitgleich.teamcraft.ui.forms

import de.zeitgleich.teamcraft.R
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class InviteFormsTest {
  @Test
  fun anEmptyAddressIsMissing() {
    assertEquals(R.string.invite_email_missing, inviteErrors("  ", "")[InviteField.EMAIL])
  }

  @Test
  fun malformedAddressesSayWhatAnAddressLooksLike() {
    for (typed in listOf("ann", "ann@", "@example.com", "ann@example", "a b@example.com", "a@b@example.com")) {
      assertEquals(typed, R.string.invite_email_invalid, inviteErrors(typed, "")[InviteField.EMAIL])
    }
  }

  @Test
  fun aPlainAddressWithANoteIsValid() {
    assertTrue(inviteErrors(" ann@example.com ", "Welcome").isValid)
  }

  @Test
  fun aLongNoteIsRefusedAtTheNote() {
    val errors = inviteErrors("ann@example.com", "x".repeat(MAX_INVITE_NOTE_CHARS + 1))
    assertEquals(InviteField.NOTE, errors.maybeFirst)
  }
}
