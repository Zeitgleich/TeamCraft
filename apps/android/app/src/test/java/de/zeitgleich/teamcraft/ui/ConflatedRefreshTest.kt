package de.zeitgleich.teamcraft.ui

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.runCurrent
import org.junit.Assert.assertEquals
import org.junit.Test

/** A burst of store changes must not queue a read per change on the core's lane (issue #1900). */
@OptIn(ExperimentalCoroutinesApi::class)
class ConflatedRefreshTest {
  @Test
  fun aBurstOfChangesCausesAtMostTwoReads() {
    val scope = TestScope(StandardTestDispatcher())
    var reads = 0
    val gate = CompletableDeferred<Unit>()
    val refresh = ConflatedRefresh(scope) {
      reads += 1
      if (reads == 1) gate.await()
    }
    repeat(50) { refresh.request() }
    scope.runCurrent()
    // The first read is still running; the other 49 changes only marked it dirty.
    assertEquals(1, reads)
    repeat(50) { refresh.request() }
    gate.complete(Unit)
    scope.advanceUntilIdle()
    assertEquals(2, reads)
  }

  @Test
  fun aChangeAfterTheReadFinishedReadsAgain() {
    val scope = TestScope(StandardTestDispatcher())
    var reads = 0
    val refresh = ConflatedRefresh(scope) { reads += 1 }
    refresh.request()
    scope.advanceUntilIdle()
    refresh.request()
    scope.advanceUntilIdle()
    assertEquals(2, reads)
  }
}
