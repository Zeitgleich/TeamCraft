package de.zeitgleich.teamcraft.push

import kotlin.coroutines.cancellation.CancellationException
import kotlinx.coroutines.delay
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import de.zeitgleich.teamcraft.core.NotSignedInException
import de.zeitgleich.teamcraft.notifications.HeldLine
import de.zeitgleich.teamcraft.notifications.HoldEnd
import de.zeitgleich.teamcraft.notifications.PushDecisionTest
import de.zeitgleich.teamcraft.notifications.PushPath
import de.zeitgleich.teamcraft.notifications.PushRetry
import uniffi.teamcraft_ffi.NotificationGap
import uniffi.teamcraft_ffi.NotificationSuppression
import uniffi.teamcraft_ffi.PushNotification
import uniffi.teamcraft_ffi.PushNotificationKind

/** Everything after a wake-up could not be read at once, with a fake core and fake jobs. */
class WakeUpRetriesTest {
  private val world = World()
  private val placeholder = PushDecisionTest.push(PushNotificationKind.MESSAGE).copy(maybeGap = NotificationGap.NOT_DECRYPTED, maybeText = null)
  private val text = PushDecisionTest.push(PushNotificationKind.MESSAGE)

  @Test
  fun aFailedRetryEndsTheHoldAndTheChainGoesOn() = runBlocking {
    world.hold()
    world.openAgain = { throw IllegalArgumentException("busy store") }

    world.retries.retry(PAYLOAD, attempt = 1, RetryOf.PLACEHOLDER, NOW)

    assertEquals(listOf(HoldEnd.ALERT), world.holdEnds)
    assertEquals(listOf(2), world.enqueued)
  }

  /** A closed core's error is an IllegalStateException too, and passes when the core opens again. */
  @Test
  fun aClosedCoreDoesNotEndTheChain() = runBlocking {
    world.hold()
    world.openAgain = { throw IllegalStateException("the object was destroyed") }

    world.retries.retry(PAYLOAD, attempt = 1, RetryOf.PLACEHOLDER, NOW)

    assertEquals(listOf(2), world.enqueued)
  }

  @Test
  fun beingSignedOutEndsTheChain() = runBlocking {
    world.hold()
    world.openAgain = { throw NotSignedInException("not signed in") }

    world.retries.retry(PAYLOAD, attempt = 1, RetryOf.PLACEHOLDER, NOW)

    assertTrue(world.enqueued.isEmpty())
  }

  @Test
  fun aCancelledRetryIsNotAFailedOne() {
    world.hold()
    world.openAgain = { throw CancellationException("the job was stopped") }

    var wasCancelled = false
    try {
      runBlocking { world.retries.retry(PAYLOAD, attempt = 1, RetryOf.PLACEHOLDER, NOW) }
    } catch (expected: CancellationException) {
      wasCancelled = true
    }

    assertTrue(wasCancelled)
    assertTrue(world.enqueued.isEmpty())
    assertTrue(world.holdEnds.isEmpty())
  }

  @Test
  fun aTimedOutRetryEndsTheHoldAndTheNextTakesOver() {
    world.hold()

    world.retries.onTimedOut(PAYLOAD, attempt = 2, RetryOf.PLACEHOLDER, NOW)

    assertEquals(listOf(HoldEnd.ALERT), world.holdEnds)
    assertEquals(listOf(3), world.enqueued)
  }

  @Test
  fun theLastFailedTryEndsTheHoldAndGivesUp() = runBlocking {
    world.hold()
    world.openAgain = { throw IllegalArgumentException("busy store") }

    world.retries.retry(PAYLOAD, attempt = PushRetry.MAX_ATTEMPTS, RetryOf.PLACEHOLDER, NOW)

    assertEquals(listOf(HoldEnd.ALERT), world.holdEnds)
    assertTrue(world.enqueued.isEmpty())
    assertEquals(1, world.gaveUp)
  }

  @Test
  fun inACarAStillUnreadableRetryKeepsTheHoldUntilTheLastTry() = runBlocking {
    world.hold()
    world.isInCar = true
    world.openAgain = { placeholder }

    world.retries.retry(PAYLOAD, attempt = 1, RetryOf.PLACEHOLDER, NOW)
    assertTrue(world.holdEnds.isEmpty())
    assertEquals(listOf(2), world.enqueued)

    world.retries.retry(PAYLOAD, attempt = PushRetry.MAX_ATTEMPTS, RetryOf.PLACEHOLDER, NOW)
    assertEquals(listOf(HoldEnd.ALERT), world.holdEnds)
    assertEquals(1, world.gaveUp)
  }

  @Test
  fun inACarAFailureEndsTheHoldToo() = runBlocking {
    world.hold()
    world.isInCar = true
    world.openAgain = { throw IllegalArgumentException("busy store") }

    world.retries.retry(PAYLOAD, attempt = 1, RetryOf.PLACEHOLDER, NOW)

    assertEquals(listOf(HoldEnd.ALERT), world.holdEnds)
  }

  /** The job without a network: a dead zone in a car still ends the hold on time. */
  @Test
  fun aHoldEndsAtItsBoundAndNotBefore() = runBlocking {
    world.hold()
    world.isInCar = true
    world.now = NOW + PushRetry.holdBoundMs(isInCar = true) - 1
    assertTrue(world.retries.endHolds())
    assertTrue(world.holdEnds.isEmpty())

    world.now = NOW + PushRetry.holdBoundMs(isInCar = true)
    assertFalse(world.retries.endHolds())
    assertEquals(listOf(HoldEnd.ALERT), world.holdEnds)
  }

  /** The car went away: nothing keeps the alert back any more. */
  @Test
  fun aForcedEndDoesNotWaitForTheBound() = runBlocking {
    world.hold()
    assertFalse(world.retries.endHolds(isForced = true))
    assertEquals(listOf(HoldEnd.ALERT), world.holdEnds)
  }

  /** What another process left: muted, read or old by now, it must not alert. */
  @Test
  fun aHoldLeftBehindLooksAtTheStateFirst() = runBlocking {
    world.now = NOW + PushRetry.holdBoundMs(isInCar = false)

    world.hold()
    world.fromStore = { placeholder.copy(maybeSuppression = NotificationSuppression.DO_NOT_DISTURB) }
    world.retries.endHolds()
    assertEquals(HoldEnd.SILENT, world.holdEnds.last())

    world.hold()
    world.fromStore = { placeholder.copy(isRead = true) }
    world.retries.endHolds()
    assertEquals(HoldEnd.DROP, world.holdEnds.last())

    world.hold()
    world.fromStore = { placeholder }
    world.isAppVisible = true
    world.retries.endHolds()
    assertEquals(HoldEnd.SILENT, world.holdEnds.last())
    world.isAppVisible = false

    world.hold()
    world.now = NOW + 6 * 60_000L
    world.retries.endHolds()
    assertEquals(HoldEnd.SILENT, world.holdEnds.last())
  }

  @Test
  fun aHoldEndFillsInWhatTheSyncBrought() = runBlocking {
    world.hold()
    world.fromStore = { text }
    world.retries.endHolds(isForced = true)
    assertEquals(listOf(PushPath.FILLED_STORE), world.filled)
  }

  /** Muted during the hold: the try right away doesn't alert for it. */
  @Test
  fun theTryRightAwayRespectsAMute() = runBlocking {
    world.hold()
    world.openAgain = { placeholder.copy(maybeSuppression = NotificationSuppression.MUTED) }

    world.retries.readSoon(PAYLOAD, placeholder)

    assertEquals(listOf(HoldEnd.SILENT), world.holdEnds)
    assertEquals(listOf(1), world.enqueued)
    assertEquals(1, world.holdEndJobs)
  }

  /**
   * A slow link: the read position costs up to 4 s more, which the try right away hasn't got, and a
   * text read must not be thrown away for it. So that try never asks; the later ones do.
   */
  @Test
  fun theTryRightAwayKeepsATextReadOnASlowLink() = runBlocking {
    world.hold()
    world.openAgain = {
      delay(SLOW_FETCH_MS)
      text
    }

    world.retries.readSoon(PAYLOAD, placeholder)

    assertEquals(listOf(PushPath.FILLED_HOLD), world.filled)
    assertEquals(1, world.openedSoon)
    assertEquals(0, world.opened)
    assertTrue(world.holdEnds.isEmpty())
  }

  @Test
  fun aLaterTryAsksHowFarTheUserHasRead() = runBlocking {
    world.hold()
    world.openAgain = { text }

    world.retries.retry(PAYLOAD, attempt = 1, RetryOf.PLACEHOLDER, NOW)

    assertEquals(1, world.opened)
    assertEquals(0, world.openedSoon)
  }

  @Test
  fun theTryRightAwayReleasesAfterAFailureButNotInACar() = runBlocking {
    world.hold()
    world.openAgain = { throw IllegalArgumentException("busy store") }
    world.retries.readSoon(PAYLOAD, placeholder)
    assertEquals(listOf(HoldEnd.ALERT), world.holdEnds)

    world.hold()
    world.isInCar = true
    world.holdEnds.clear()
    world.retries.readSoon(PAYLOAD, placeholder)
    assertTrue(world.holdEnds.isEmpty())
  }

  @Test
  fun aPlaceholderThatWentCostsNoFetch() = runBlocking {
    world.openAgain = { error("no fetch expected") }

    world.retries.retry(PAYLOAD, attempt = 1, RetryOf.PLACEHOLDER, NOW)

    assertEquals(0, world.opened)
    assertTrue(world.enqueued.isEmpty())
  }

  @Test
  fun aFullQueueStartsNoNewChainButFinishesRunningOnes() = runBlocking {
    world.pending = world.maxChains
    world.retries.afterNotOpened(PAYLOAD, wasAnnounced = true)
    assertTrue(world.enqueued.isEmpty())
    assertEquals(1, world.capped)

    world.hold()
    world.openAgain = { placeholder }
    world.retries.retry(PAYLOAD, attempt = 1, RetryOf.PLACEHOLDER, NOW)
    assertEquals(listOf(2), world.enqueued)
  }

  @Test
  fun aWakeUpThatDidNotOpenSaysWhetherAnythingWentOut() = runBlocking {
    world.openAgain = { text }
    world.retries.retry(PAYLOAD, attempt = 1, RetryOf.UNOPENED_QUIET, NOW)
    world.retries.retry(PAYLOAD, attempt = 1, RetryOf.UNOPENED_ANNOUNCED, NOW)
    assertEquals(listOf(false, true), world.openedLate)
  }

  private class World {
    var openAgain: suspend () -> PushNotification = { error("not set") }
    var fromStore: suspend () -> PushNotification = { throw IllegalStateException("no core") }
    val holdEnds = mutableListOf<HoldEnd>()
    val enqueued = mutableListOf<Int>()
    val filled = mutableListOf<PushPath>()
    val openedLate = mutableListOf<Boolean>()
    var isInCar = false
    var isAppVisible = false
    var now = NOW
    var pending = 0
    val maxChains = 3
    var opened = 0
    var openedSoon = 0
    var gaveUp = 0
    var capped = 0
    var holdEndJobs = 0
    private var held = listOf<HeldLine>()

    /** A placeholder of [PAYLOAD] shows, with its alert held. */
    fun hold() {
      held = listOf(HeldLine(PAYLOAD, NOW))
    }

    val retries = WakeUpRetries(
      opener = object : WakeUpOpener {
        override suspend fun openSoon(payload: ByteArray): PushNotification {
          openedSoon += 1
          return openAgain()
        }

        override suspend fun openLate(payload: ByteArray): PushNotification {
          opened += 1
          return openAgain()
        }

        override suspend fun fromStore(payload: ByteArray): PushNotification = fromStore()
      },
      target = object : RetryTarget {
        override fun handleRetry(notification: PushNotification, path: PushPath) {
          if (notification.maybeGap == null) {
            filled += path
            held = emptyList()
          }
        }

        override fun handleOpenedLate(notification: PushNotification, payload: ByteArray, wasAnnounced: Boolean, arrivedAtMs: Long) {
          openedLate += wasAnnounced
        }

        override fun isPlaceholderShowing(notification: PushNotification) = held.isNotEmpty()

        override fun hasPlaceholderFor(payload: ByteArray) = held.isNotEmpty()

        override fun heldLines() = held

        override fun endHold(payload: ByteArray, end: HoldEnd) {
          holdEnds += end
          // The line stays a placeholder, without a hold; the fake tells that by what it keeps.
          held = emptyList()
        }

        override fun recordGaveUp() {
          gaveUp += 1
        }

        override fun recordCapped() {
          capped += 1
        }
      },
      scheduler = object : RetryScheduler {
        override fun enqueueRetry(payload: ByteArray, attempt: Int, retryOf: RetryOf, arrivedAtMs: Long) {
          enqueued += attempt
        }

        override fun scheduleHoldEnd(delayMs: Long) {
          holdEndJobs += 1
        }

        override fun pendingChains() = pending

        override fun maxChains() = maxChains
      },
      isInCar = { isInCar },
      isAppVisible = { isAppVisible },
      isSigningOut = { false },
      elapsedNow = { now },
      log = {},
    )
  }

  private companion object {
    const val NOW = 1_000_000L
    const val SLOW_FETCH_MS = 50L
    val PAYLOAD = byteArrayOf(1, 2, 3)
  }
}
