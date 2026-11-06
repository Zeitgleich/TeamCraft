package de.zeitgleich.teamcraft.security

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class AppLockGateTest {
  private val key = FakeKey()
  private var hasScreenLock = true
  private val gate = AppLockGate(key) { hasScreenLock }

  @Test
  fun anAuthorizedProofUnlocks() {
    val challenge = promptedChallenge()
    key.authorize(challenge)
    assertTrue(gate.isUnlocked(challenge, challenge))
  }

  @Test
  fun aForgedSuccessWithoutAProofIsRefused() {
    val challenge = promptedChallenge()
    assertFalse(gate.isUnlocked(challenge, maybeProof = null))
  }

  @Test
  fun aForgedSuccessWithTheUnauthorizedChallengeIsRefused() {
    val challenge = promptedChallenge()
    assertFalse(gate.isUnlocked(challenge, challenge))
  }

  @Test
  fun aForgedSuccessWithAnotherAuthorizedChallengeIsRefused() {
    val earlier = promptedChallenge()
    key.authorize(earlier)
    val challenge = promptedChallenge()
    assertFalse(gate.isUnlocked(challenge, earlier))
  }

  @Test
  fun noScreenLockOpensAndDropsTheKey() {
    promptedChallenge()
    hasScreenLock = false
    assertEquals(UnlockStart.Open, gate.start())
    assertFalse(key.exists)
  }

  @Test
  fun anInvalidatedKeyIsReplacedBeforeThePrompt() {
    promptedChallenge()
    key.isInvalidated = true
    val challenge = promptedChallenge()
    assertEquals(2, key.generations)
    key.authorize(challenge)
    assertTrue(gate.isUnlocked(challenge, challenge))
  }

  @Test
  fun theFirstUnlockAfterAnUpdateCreatesTheKey() {
    // An install from before #1200 has the lock on but no key.
    assertFalse(key.exists)
    promptedChallenge()
    assertTrue(key.exists)
    assertEquals(1, key.generations)
  }

  @Test
  fun aBrokenKeystoreFallsBackToThePromptAlone() {
    key.maybeFailure = IllegalStateException("keystore")
    assertTrue(gate.start() is UnlockStart.PromptUnbound)
  }

  private fun promptedChallenge(): Challenge {
    val start = gate.start()
    check(start is UnlockStart.Prompt) { "expected a prompt, got $start" }
    return start.challenge
  }

  /** Stands for an operation of the Keystore key: usable only once the prompt authorized it. */
  class Challenge(val generation: Int) {
    var isAuthorized = false
  }

  /** The Keystore's rules for [AppLockKey], without a Keystore. */
  class FakeKey : UnlockKey<Challenge> {
    var exists = false
    var generations = 0
    var isInvalidated = false
    var maybeFailure: Exception? = null

    override fun begin(): Challenge {
      maybeFailure?.let { throw it }
      if (!exists) {
        exists = true
        isInvalidated = false
        generations += 1
      }
      if (isInvalidated) throw UnlockKeyInvalidatedException(IllegalStateException("invalidated"))
      return Challenge(generations)
    }

    override fun isProof(challenge: Challenge, maybeProof: Challenge?): Boolean =
      maybeProof === challenge && challenge.isAuthorized && challenge.generation == generations && !isInvalidated

    override fun reset() {
      exists = false
    }

    /** What a real prompt success does to the operation handed to it. */
    fun authorize(challenge: Challenge) {
      challenge.isAuthorized = true
    }
  }
}
