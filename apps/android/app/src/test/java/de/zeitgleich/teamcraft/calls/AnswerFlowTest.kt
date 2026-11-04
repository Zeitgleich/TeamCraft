package de.zeitgleich.teamcraft.calls

import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import kotlin.concurrent.thread
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** Answering a ring (#647, #1272): which path it takes, and that Telecom's answer callback never waits for the join. */
class AnswerFlowTest {
  @Test
  fun answeringFromTheAppAnswersTheTelecomCallFirst() {
    assertEquals(AnswerRoute.ANSWER_TELECOM_THEN_JOIN, route(AnswerSource.APP, hasTelecomCall = true))
  }

  @Test
  fun answeringWhileTelecomStillAddsTheRingJoinsWithoutWaiting() {
    assertEquals(AnswerRoute.JOIN, route(AnswerSource.APP, isTelecomAdding = true))
  }

  @Test
  fun answeringWithoutATelecomCallJoinsInAnOutgoingOne() {
    assertEquals(AnswerRoute.ADD_TELECOM_CALL, route(AnswerSource.APP))
  }

  @Test
  fun telecomAnsweredItsCallAlreadySoItOnlyJoins() {
    assertEquals(AnswerRoute.JOIN, route(AnswerSource.TELECOM, hasTelecomCall = true))
  }

  @Test
  fun aSecondAnswerDoesNothing() {
    assertEquals(AnswerRoute.IGNORE, route(AnswerSource.APP, isJoining = true, hasTelecomCall = true))
    assertEquals(AnswerRoute.IGNORE, route(AnswerSource.TELECOM, isInCall = true, hasTelecomCall = true))
  }

  /**
   * The regression of #1272: Telecom's onAnswer ran the whole join (call screen, microphone, codec,
   * connecting), Telecom gives it 5 s and then tears the call down. Starting an answer must return
   * while the join is still connecting.
   */
  @Test
  fun startingAnAnswerReturnsWhileTheJoinStillConnects() = withScope { scope ->
    val steps = FakeSteps(joinResult = CompletableDeferred())
    var maybeJob: Job? = null
    val returned = CountDownLatch(1)
    // On its own thread: an answer that waited for the join would never return, not hang the test.
    thread(isDaemon = true) {
      maybeJob = startAnswer(scope, AnswerRoute.JOIN, steps)
      returned.countDown()
    }
    assertTrue("startAnswer waited for the join", returned.await(1, TimeUnit.SECONDS))
    runBlocking { withTimeout(1_000) { steps.joinStarted.await() } }
    assertTrue(maybeJob?.isActive == true)
    assertFalse(steps.isEnded)
  }

  @Test
  fun anAnswerFromTheAppAnswersTelecomBeforeJoining() = withScope { scope ->
    val steps = FakeSteps(joinResult = CompletableDeferred(true))
    runBlocking { startAnswer(scope, AnswerRoute.ANSWER_TELECOM_THEN_JOIN, steps)?.join() }
    assertEquals(listOf("answer", "join"), steps.calls)
  }

  @Test
  fun anAnswerFromTelecomDoesNotAnswerTelecomAgain() = withScope { scope ->
    val steps = FakeSteps(joinResult = CompletableDeferred(true))
    runBlocking { startAnswer(scope, AnswerRoute.JOIN, steps)?.join() }
    assertEquals(listOf("join"), steps.calls)
  }

  /** A failed join ends the Telecom call, the service and its notification (no stuck call, #647). */
  @Test
  fun aFailedJoinEndsTheCall() = withScope { scope ->
    val steps = FakeSteps(joinResult = CompletableDeferred(false))
    runBlocking { startAnswer(scope, AnswerRoute.JOIN, steps)?.join() }
    assertEquals(listOf("join", "end"), steps.calls)
  }

  @Test
  fun withoutATelecomCallTheOutgoingOneJoins() = withScope { scope ->
    val steps = FakeSteps(joinResult = CompletableDeferred(true))
    runBlocking { startAnswer(scope, AnswerRoute.ADD_TELECOM_CALL, steps)?.join() }
    assertEquals(listOf("add"), steps.calls)
  }

  @Test
  fun anIgnoredAnswerDoesNothing() = withScope { scope ->
    val steps = FakeSteps(joinResult = CompletableDeferred(true))
    assertNull(startAnswer(scope, AnswerRoute.IGNORE, steps))
    assertTrue(steps.calls.isEmpty())
  }

  /** With the app in front the full-screen intent is only a heads-up: the app opens the screen itself. */
  @Test
  fun aVerifiedRingOpensTheIncomingScreenOverTheOpenApp() {
    assertTrue(ring(isVerified = true).opensScreenAtOnce(isAppVisible = true))
    assertFalse(ring(isVerified = true).opensScreenAtOnce(isAppVisible = false))
    assertFalse(ring(isVerified = false).opensScreenAtOnce(isAppVisible = true))
  }

  private fun route(
    source: AnswerSource,
    isInCall: Boolean = false,
    isJoining: Boolean = false,
    hasTelecomCall: Boolean = false,
    isTelecomAdding: Boolean = false,
  ) = AnswerRoute.of(source, isInCall, isJoining, hasTelecomCall, isTelecomAdding)

  private fun ring(isVerified: Boolean) =
    IncomingRing("1", maybeCallId = "7", displayName = "planning", maybeCallerName = "Alice", isVerified = isVerified)

  private fun withScope(test: (CoroutineScope) -> Unit) {
    val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    try {
      test(scope)
    } finally {
      scope.cancel()
    }
  }

  private class FakeSteps(private val joinResult: CompletableDeferred<Boolean>) : AnswerSteps {
    val calls: MutableList<String> = java.util.Collections.synchronizedList(mutableListOf())
    val joinStarted = CompletableDeferred<Unit>()
    val isEnded: Boolean get() = "end" in calls

    override suspend fun answerTelecomCall() {
      calls += "answer"
    }

    override suspend fun join(): Boolean {
      calls += "join"
      joinStarted.complete(Unit)
      return joinResult.await()
    }

    override suspend fun addTelecomCall() {
      calls += "add"
    }

    override suspend fun endCall() {
      calls += "end"
    }
  }
}
