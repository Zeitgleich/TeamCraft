package de.zeitgleich.teamcraft.security

import de.zeitgleich.teamcraft.auth.ServerProofOffer
import java.math.BigInteger
import java.security.KeyFactory
import java.security.KeyPair
import java.security.KeyPairGenerator
import java.security.Signature
import java.security.interfaces.ECPublicKey
import java.security.spec.ECGenParameterSpec
import java.security.spec.ECPoint
import java.security.spec.ECPublicKeySpec
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.teamcraft_ffi.DeviceProofException

private val MESSAGE = "header.payload".encodeToByteArray()

class DeviceProofKeysTest {
  class Es256Encoding {
    @Test
    fun aDerSignatureBecomesRAndSThatVerify() {
      repeat(50) {
        val pair = newPair()
        val raw = Es256.sign(pair.private, MESSAGE)
        assertEquals(64, raw.size)
        val verifier = Signature.getInstance("SHA256withECDSAinP1363Format")
        verifier.initVerify(pair.public)
        verifier.update(MESSAGE)
        assertTrue(verifier.verify(raw))
      }
    }

    @Test
    fun shortAndSignedValuesArePaddedToThirtyTwoBytes() {
      val r = BigInteger.ONE
      val s = BigInteger.ONE.shiftLeft(255)
      val der = der(r, s)
      val raw = Es256.rawSignature(der)
      assertEquals(64, raw.size)
      assertEquals(r, BigInteger(1, raw.copyOfRange(0, 32)))
      assertEquals(s, BigInteger(1, raw.copyOfRange(32, 64)))
    }

    @Test
    fun malformedSignaturesAreRefused() {
      val good = der(BigInteger.TEN, BigInteger.TEN)
      for (bad in listOf(
        ByteArray(0),
        good.copyOf(good.size - 1),
        good + byteArrayOf(0),
        byteArrayOf(0x31) + good.copyOfRange(1, good.size),
        der(BigInteger.ONE.shiftLeft(256), BigInteger.ONE),
        der(BigInteger.ZERO, BigInteger.ONE),
      )) {
        assertThrows(Exception::class.java) { Es256.rawSignature(bad) }
      }
    }

    @Test
    fun thePublicPointIsUncompressedAndNamesTheKey() {
      repeat(20) {
        val public = newPair().public as ECPublicKey
        val point = Es256.uncompressedPoint(public)
        assertEquals(65, point.size)
        assertEquals(0x04.toByte(), point[0])
        val rebuilt = KeyFactory.getInstance("EC").generatePublic(
          ECPublicKeySpec(
            ECPoint(BigInteger(1, point.copyOfRange(1, 33)), BigInteger(1, point.copyOfRange(33, 65))),
            public.params,
          ),
        )
        assertArrayEquals(public.encoded, rebuilt.encoded)
      }
    }

    @Test
    fun aSoftwareKeyRecordRoundTripsAndADamagedOneIsRefused() {
      val (public, private) = SoftwareKeyRecord.decode(SoftwareKeyRecord.encode(byteArrayOf(1, 2, 3), byteArrayOf(4, 5)))
      assertArrayEquals(byteArrayOf(1, 2, 3), public)
      assertArrayEquals(byteArrayOf(4, 5), private)
      assertThrows(IllegalArgumentException::class.java) { SoftwareKeyRecord.decode(byteArrayOf(2, 0, 1, 9, 9)) }
      assertThrows(IllegalArgumentException::class.java) { SoftwareKeyRecord.decode(byteArrayOf(1, 0, 9, 9)) }
    }
  }

  class KeyStoreChoices {
    @Test
    fun aKeyIsMadeInHardwareAndKeptForTheNextUse() {
      val vault = FakeVault()
      val store = DeviceKeyStore(vault) {}
      val key = store.keyOrNew()
      assertSame(key, store.keyOrNew())
      assertEquals(listOf("hardware"), vault.made)
    }

    @Test
    fun aKeystoreThatRefusesGetsASoftwareKey() {
      val logged = mutableListOf<String>()
      val vault = FakeVault(isHardwareRefused = true)
      DeviceKeyStore(vault) { logged += it }.keyOrNew()
      assertEquals(listOf("software"), vault.made)
      assertTrue(logged.single().contains("software key"))
    }

    @Test
    fun aStoreThatCantBeReadReplacesNothing() {
      val vault = FakeVault().apply { failure = DeviceKeyException.Unreadable() }
      assertThrows(DeviceKeyException.Unreadable::class.java) { DeviceKeyStore(vault) {}.keyOrNew() }
      assertTrue(vault.made.isEmpty())
    }

    @Test
    fun aKeyTheKeystoreCallsGoneIsReplaced() {
      val vault = FakeVault().apply { failure = DeviceKeyException.Gone() }
      DeviceKeyStore(vault) {}.keyOrNew()
      assertEquals(1, vault.deletes)
      assertEquals(listOf("hardware"), vault.made)
    }
  }

  class WhatTheCoreIsTold {
    @Test
    fun aKeySignsWithItsPublicHalf() {
      val vault = FakeVault()
      DeviceKeyStore(vault) {}.keyOrNew()
      val key = VaultProofKey(vault)
      assertEquals(65, key.publicKey().size)
      assertEquals(64, key.sign(MESSAGE).size)
    }

    @Test
    fun noKeyIsALostKey() {
      val key = VaultProofKey(FakeVault())
      assertThrows(DeviceProofException.KeyLost::class.java) { key.publicKey() }
      assertThrows(DeviceProofException.KeyLost::class.java) { key.sign(MESSAGE) }
    }

    @Test
    fun aKeyTheKeystoreCallsGoneIsALostKey() {
      val vault = FakeVault().apply { failure = DeviceKeyException.Gone() }
      assertThrows(DeviceProofException.KeyLost::class.java) { VaultProofKey(vault).publicKey() }
    }

    @Test
    fun aStoreThatCantBeReadIsAWaitNotALoss() {
      val vault = FakeVault().apply { failure = DeviceKeyException.Unreadable() }
      assertThrows(DeviceProofException.KeyUnavailable::class.java) { VaultProofKey(vault).publicKey() }
      vault.failure = IllegalStateException("keystore restarting")
      assertThrows(DeviceProofException.KeyUnavailable::class.java) { VaultProofKey(vault).sign(MESSAGE) }
    }

    @Test
    fun aSignatureThatFailsIsAWaitUnlessTheKeyIsGone() {
      val vault = FakeVault()
      DeviceKeyStore(vault) {}.keyOrNew()
      vault.signFailure = DeviceKeyException.Unreadable()
      assertThrows(DeviceProofException.KeyUnavailable::class.java) { VaultProofKey(vault).sign(MESSAGE) }
      vault.signFailure = DeviceKeyException.Gone()
      assertThrows(DeviceProofException.KeyLost::class.java) { VaultProofKey(vault).sign(MESSAGE) }
    }

    @Test
    fun aSignOutLeavesNoKey() {
      val vault = FakeVault()
      val store = DeviceKeyStore(vault) {}
      store.keyOrNew()
      store.delete()
      assertNull(vault.read())
      assertThrows(DeviceProofException.KeyLost::class.java) { store.proofKey.publicKey() }
    }
  }

  class ServerOffers {
    @Test
    fun onlyAFreshAnswerWithANonceOffersProofs() {
      assertEquals(ServerProofOffer.OFFERED, ServerProofOffer.of(200, "nonce"))
      assertEquals(ServerProofOffer.NOT_OFFERED, ServerProofOffer.of(200, null))
      assertEquals(ServerProofOffer.NOT_OFFERED, ServerProofOffer.of(404, null))
      assertEquals(ServerProofOffer.NOT_OFFERED, ServerProofOffer.of(421, "nonce"))
      assertEquals(ServerProofOffer.NO_ANSWER, ServerProofOffer.of(502, "nonce"))
      assertEquals(ServerProofOffer.NO_ANSWER, ServerProofOffer.of(302, null))
    }
  }
}

/** A vault in memory, with software keys standing in for the Keystore's. */
private class FakeVault(private val isHardwareRefused: Boolean = false) : DeviceKeyVault {
  var failure: Exception? = null
  var signFailure: Exception? = null
  var deletes = 0
  val made = mutableListOf<String>()
  private var maybeKey: StoredDeviceKey? = null

  override fun read(): StoredDeviceKey? {
    failure?.let { throw it }
    return maybeKey
  }

  override fun createInHardware(): StoredDeviceKey {
    if (isHardwareRefused) throw IllegalStateException("no EC keys here")
    return make("hardware")
  }

  override fun createInSoftware(): StoredDeviceKey = make("software")

  override fun delete() {
    deletes += 1
    failure = null
    maybeKey = null
  }

  private fun make(kind: String): StoredDeviceKey {
    made += kind
    val pair = newPair()
    val key = object : StoredDeviceKey {
      override val publicPoint = Es256.uncompressedPoint(pair.public as ECPublicKey)

      override fun sign(message: ByteArray): ByteArray {
        signFailure?.let { throw it }
        return Es256.sign(pair.private, message)
      }
    }
    maybeKey = key
    return key
  }
}

private fun newPair(): KeyPair =
  KeyPairGenerator.getInstance("EC").apply { initialize(ECGenParameterSpec("secp256r1")) }.generateKeyPair()

/** A DER `ECDSA-Sig-Value` of [r] and [s]. */
private fun der(r: BigInteger, s: BigInteger): ByteArray {
  fun integer(value: BigInteger): ByteArray {
    val bytes = value.toByteArray()
    return byteArrayOf(0x02, bytes.size.toByte()) + bytes
  }
  val body = integer(r) + integer(s)
  return byteArrayOf(0x30, body.size.toByte()) + body
}
