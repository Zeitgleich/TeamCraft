package de.zeitgleich.teamcraft.core

import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

/** #2184: a call that can block fails loudly on the main thread in debug builds, and is reported once in release builds. */
class UiThreadGuardTest {
  @Test
  fun offTheMainThreadNothingHappens() {
    val reports = mutableListOf<String>()
    val guard = UiThreadGuard(isOnUiThread = { false }, failsLoudly = true, report = { reports += it })
    guard.check("Tokens.accessToken")
    assertTrue(reports.isEmpty())
  }

  @Test
  fun aDebugBuildFailsLoudlyOnTheMainThread() {
    val guard = UiThreadGuard(isOnUiThread = { true }, failsLoudly = true, report = {})
    val failure = assertThrows(IllegalStateException::class.java) { guard.check("Tokens.accessToken") }
    assertTrue(failure.message!!.contains("Tokens.accessToken"))
  }

  @Test
  fun aReleaseBuildReportsEachCallSiteOnceAndGoesOn() {
    val reports = mutableListOf<String>()
    val guard = UiThreadGuard(isOnUiThread = { true }, failsLoudly = false, report = { reports += it })
    repeat(3) { guard.check("Tokens.accessToken") }
    guard.check("CoreHttpConnection.answer")
    assertEquals(listOf("Tokens.accessToken", "CoreHttpConnection.answer"), reports)
  }
}
