package de.zeitgleich.teamcraft.ui

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class DetectedTest {
  @Test
  fun findsPhoneNumbersEmailAndAddresses() {
    val text = "Call +49 170 1234567 or write to anna@example.com. Visit Hauptstrasse 12, 10115 Berlin."
    val found = findDetected(text)
    assertEquals(listOf(DetectedKind.PHONE, DetectedKind.EMAIL, DetectedKind.ADDRESS), found.map { it.kind })
    assertEquals("tel:+491701234567", found[0].uri)
    assertEquals("mailto:anna@example.com", found[1].uri)
    assertTrue(found[2].uri.startsWith("geo:0,0?q="))
  }

  @Test
  fun datesTimesAndShortNumbersAreNotPhoneNumbers() {
    assertEquals(emptyList<Detected>(), findDetected("On 2026-11-15 at 10:30, ticket 123456, 15.11.2026"))
  }

  @Test
  fun nothingIsFoundInsideCode() {
    assertEquals(emptyList<Detected>(), findDetected("run `call 0170 1234567` or\n```\nmail a@example.com\n```"))
  }

  @Test
  fun anEmailInsideALinkStaysPartOfTheLink() {
    val found = findActionable("see https://example.com/?to=a@example.com now")
    assertEquals(listOf(DetectedKind.WEB), found.map { it.kind })
  }
}
