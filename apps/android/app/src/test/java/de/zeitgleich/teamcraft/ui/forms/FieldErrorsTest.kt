package de.zeitgleich.teamcraft.ui.forms

import de.zeitgleich.teamcraft.R
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

private const val GOOD_PASSPHRASE = "correct horse battery"

class FieldErrorsTest {
  class Errors {
    @Test
    fun fieldsWithoutAnErrorAreLeftOut() {
      val errors = FieldErrors.of(NewPassphraseField.PASSPHRASE to null, NewPassphraseField.REPEATED to null)
      assertTrue(errors.isValid)
      assertNull(errors.maybeFirst)
    }

    @Test
    fun theFirstInvalidFieldIsTheTopmost() {
      val errors = FieldErrors.of(
        NewPassphraseField.REPEATED to R.string.passphrases_differ,
        NewPassphraseField.PASSPHRASE to R.string.passphrase_too_short,
      )
      assertEquals(NewPassphraseField.PASSPHRASE, errors.maybeFirst)
    }

    @Test
    fun aServerRefusalReplacesTheFieldsError() {
      val errors = FieldErrors.of(NewPassphraseField.PASSPHRASE to null)
        .withServerError(NewPassphraseField.PASSPHRASE, R.string.error_wrong_passphrase)
      assertEquals(R.string.error_wrong_passphrase, errors[NewPassphraseField.PASSPHRASE])
      assertEquals(errors, errors.withServerError(NewPassphraseField.REPEATED, null))
    }
  }

  class Passphrase {
    @Test
    fun bothErrorsShowAtOnce() {
      val errors = newPassphraseErrors("short", "")
      assertEquals(R.string.passphrase_too_short, errors[NewPassphraseField.PASSPHRASE])
      assertEquals(R.string.passphrase_repeat_missing, errors[NewPassphraseField.REPEATED])
      assertEquals(NewPassphraseField.PASSPHRASE, errors.maybeFirst)
    }

    @Test
    fun aDifferentRepetitionIsSaidAtTheSecondField() {
      val errors = newPassphraseErrors(GOOD_PASSPHRASE, "$GOOD_PASSPHRASE!")
      assertNull(errors[NewPassphraseField.PASSPHRASE])
      assertEquals(R.string.passphrases_differ, errors[NewPassphraseField.REPEATED])
    }

    @Test
    fun aLongEnoughRepeatedPassphraseIsValid() {
      assertTrue(newPassphraseErrors(GOOD_PASSPHRASE, GOOD_PASSPHRASE).isValid)
    }

    @Test
    fun aSinglePassphraseIsMissingOrTooShort() {
      assertEquals(R.string.passphrase_missing, passphraseError(""))
      assertEquals(R.string.passphrase_too_short, passphraseError("a".repeat(MIN_PASSPHRASE_LENGTH - 1)))
      assertNull(passphraseError("a".repeat(MIN_PASSPHRASE_LENGTH)))
    }

    @Test
    fun onlyAWrongPassphraseGoesToTheField() {
      assertTrue(isPassphraseRefusal(R.string.error_wrong_passphrase))
      assertFalse(isPassphraseRefusal(R.string.error_network))
      assertFalse(isPassphraseRefusal(R.string.error_backup_damaged))
    }
  }

  class Codes {
    @Test
    fun theNewDevicesCodeHasSixDigits() {
      assertEquals(R.string.approve_code_missing, linkCodeError(""))
      assertEquals(R.string.approve_code_incomplete, linkCodeError("12345"))
      assertNull(linkCodeError("123456"))
    }

    @Test
    fun aLinkCodeMustBeTyped() {
      assertEquals(R.string.approve_link_code_missing, deviceLinkCodeError("  "))
      assertNull(deviceLinkCodeError("ABCD-EFGH"))
    }
  }

  class Texts {
    @Test
    fun aBlankTextIsRefused() {
      assertEquals(R.string.channel_name_missing, requiredTextError(" ", R.string.channel_name_missing))
      assertNull(requiredTextError("general", R.string.channel_name_missing))
    }

    @Test
    fun theConfirmCountIsAWholeNumberInRange() {
      assertNull(confirmOverCount(""))
      assertNull(confirmOverCount("100000"))
      assertEquals(0, confirmOverCount("0"))
      assertEquals(MAX_CONFIRM_OVER, confirmOverCount("99999"))
    }
  }
}
