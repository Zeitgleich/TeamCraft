package de.zeitgleich.teamcraft.security

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class AppLockGateTest {
  private val key = FakeKey()
  private val gate = AppLockGate(key)

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
    assertEquals(UnlockStart.Open, gate.start(hasScreenLock = false))
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
    assertTrue(gate.start(hasScreenLock = true) is UnlockStart.PromptUnbound)
  }

  @Test
  fun aBrokenKeyAtTheProofIsReplacedForTheNextPrompt() {
    val challenge = promptedChallenge()
    key.authorize(challenge)
    key.isBrokenAtProof = true
    assertFalse(gate.isUnlocked(challenge, challenge))
    assertFalse(key.exists)
    key.isBrokenAtProof = false
    val next = promptedChallenge()
    assertEquals(2, key.generations)
    key.authorize(next)
    assertTrue(gate.isUnlocked(next, next))
  }

  @Test
  fun proofsThatKeepFailingAfterAPromptFallBackToThePromptAlone() {
    // A Keystore that refuses every proof although the system said "success".
    repeat(AppLockGate.MAX_REFUSED_PROOFS) {
      val challenge = promptedChallenge()
      key.authorize(challenge)
      key.isRefusingEverything = true
      assertFalse(gate.isUnlocked(challenge, challenge))
    }
    val start = gate.start(hasScreenLock = true)
    assertTrue(start is UnlockStart.PromptUnbound)
    assertTrue((start as UnlockStart.PromptUnbound).failure is ProofsRefusedException)
  }

  @Test
  fun aValidProofClearsTheRefusals() {
    repeat(AppLockGate.MAX_REFUSED_PROOFS - 1) {
      val forged = promptedChallenge()
      assertFalse(gate.isUnlocked(forged, forged))
    }
    val challenge = promptedChallenge()
    key.authorize(challenge)
    assertTrue(gate.isUnlocked(challenge, challenge))
    val forged = promptedChallenge()
    assertFalse(gate.isUnlocked(forged, forged))
    assertTrue(gate.start(hasScreenLock = true) is UnlockStart.Prompt)
  }

  private fun promptedChallenge(): Challenge {
    val start = gate.start(hasScreenLock = true)
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
    var isBrokenAtProof = false
    var isRefusingEverything = false

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

    override fun prove(challenge: Challenge, maybeProof: Challenge?): Proof = when {
      isBrokenAtProof -> Proof.KeyBroken
      isRefusingEverything -> Proof.Refused
      maybeProof !== challenge || !challenge.isAuthorized -> Proof.Refused
      challenge.generation != generations || isInvalidated -> Proof.KeyBroken
      else -> Proof.Valid
    }

    override fun reset() {
      exists = false
    }

    /** What a real prompt success does to the operation handed to it. */
    fun authorize(challenge: Challenge) {
      challenge.isAuthorized = true
    }
  }
}
