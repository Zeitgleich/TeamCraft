package de.zeitgleich.teamcraft.ui

import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import org.junit.Assert.assertEquals
import org.junit.Test

@OptIn(ExperimentalCoroutinesApi::class)
class PictureRetriesTest {
  private val scope = TestScope(StandardTestDispatcher())
  private val online = MutableStateFlow(true)
  private val rounds = FakeRounds()
  private val retries = PictureRetries(scope, online, rounds).also { scope.runCurrent() }

  @Test
  fun everythingThatFailedAsksAgainInOneRound() {
    retries.failed()
    retries.failed()
    retries.failed()
    assertEquals("one round is planned for all of them", listOf<Long?>(5_000, null, null), rounds.answers)
    scope.advanceTimeBy(4_999)
    assertEquals(0, retries.round.value)
    scope.advanceTimeBy(1)
    scope.runCurrent()
    assertEquals(1, retries.round.value)
    assertEquals(1, rounds.started)
  }

  @Test
  fun aFailureAfterTheRoundPlansTheNextOneLonger() {
    retries.failed()
    scope.advanceTimeBy(5_000)
    scope.runCurrent()
    retries.failed()
    scope.advanceTimeBy(10_000)
    scope.runCurrent()
    assertEquals(2, retries.round.value)
    assertEquals(listOf<Long?>(5_000, 10_000), rounds.answers)
  }

  @Test
  fun theEndOfAnOutageRunsTheRoundAtOnceAndStartsTheBackoffOver() {
    retries.failed()
    online.value = false
    scope.runCurrent()
    assertEquals(0, retries.round.value)
    online.value = true
    scope.runCurrent()
    assertEquals(1, retries.round.value)
    assertEquals(1, rounds.restarts)
    scope.advanceTimeBy(60_000)
    scope.runCurrent()
    assertEquals("the planned round was dropped, it doesn't run twice", 1, retries.round.value)
    assertEquals(0, rounds.started)
  }

  @Test
  fun comingOnlineWithNothingWaitingAsksForNothing() {
    online.value = false
    scope.runCurrent()
    online.value = true
    scope.runCurrent()
    assertEquals(0, retries.round.value)
    assertEquals(0, rounds.restarts)
  }

  @Test
  fun nothingRunsWhileNoPictureWaits() {
    scope.advanceTimeBy(24 * 60 * 60 * 1000L)
    scope.runCurrent()
    assertEquals(0, retries.round.value)
    assertEquals(emptyList<Long?>(), rounds.answers)
  }

  @Test
  fun theServersWaitGoesToTheCore() {
    retries.failed(maybeServerWaitMs = 30_000)
    assertEquals(listOf<Long?>(30_000), rounds.answers)
  }

  @Test
  fun aSuccessGoesToTheCore() {
    retries.succeeded()
    assertEquals(1, rounds.successes)
  }

  /** Doubling without jitter up to the ceiling, like the core's policy; one round at a time. */
  private class FakeRounds : RetryRounds {
    val answers = mutableListOf<Long?>()
    var started = 0
    var restarts = 0
    var successes = 0
    private var isWaiting = false
    private var failures = 0

    override fun failed(maybeServerWaitMs: Long?): Long? {
      val answer = if (isWaiting) {
        null
      } else {
        isWaiting = true
        maxOf(minOf(5_000L shl failures++, 300_000L), maybeServerWaitMs ?: 0)
      }
      answers += answer
      return answer
    }

    override fun roundStarted() {
      started++
      isWaiting = false
    }

    override fun succeeded() {
      successes++
      failures = 0
    }

    override fun restart() {
      restarts++
      isWaiting = false
      failures = 0
    }
  }
}
