package de.zeitgleich.teamcraft

import android.util.Base64
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import java.io.File
import java.math.BigInteger
import java.security.KeyFactory
import java.security.KeyPairGenerator
import java.security.Signature
import java.security.interfaces.ECPublicKey
import java.security.spec.ECGenParameterSpec
import java.security.spec.ECPoint
import java.security.spec.ECPublicKeySpec
import kotlinx.coroutines.runBlocking
import org.json.JSONObject
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import de.zeitgleich.teamcraft.security.DeviceKeyStore
import de.zeitgleich.teamcraft.security.KeystoreBox
import de.zeitgleich.teamcraft.security.KeystoreDeviceKeyVault
import de.zeitgleich.teamcraft.security.SealedFile
import uniffi.teamcraft_ffi.DeviceProofException
import uniffi.teamcraft_ffi.ProviderProver
import uniffi.teamcraft_ffi.TokenEndpointAnswer
import uniffi.teamcraft_ffi.TokenEndpointTransport
import uniffi.teamcraft_ffi.TokenProofFallback

private const val ENDPOINT = "https://id.example.com/realms/team/protocol/openid-connect/token"

/**
 * The device key for DPoP proofs in the real Keystore, signing for the real core (e2ee.md §3.1.2):
 * the core's proofs carry the Keystore key and verify with it, a nonce challenge gets one more try,
 * and a deleted key is a lost key, not a wait.
 */
@RunWith(AndroidJUnit4::class)
class DeviceKeyOnDeviceTest {
  private val context = InstrumentationRegistry.getInstrumentation().targetContext
  private val alias = "teamcraft_dpop.test-${System.nanoTime()}"
  private lateinit var file: File
  private lateinit var keys: DeviceKeyStore

  @Before
  fun createStore() {
    file = File(context.cacheDir, "device-key-test-${System.nanoTime()}.bin")
    keys = DeviceKeyStore(KeystoreDeviceKeyVault(alias, SealedFile(file, KeystoreBox("teamcraft_test_device_key"))))
  }

  @After
  fun deleteKey() {
    keys.delete()
  }

  @Test
  fun theCoresProofsAreSignedByTheKeystoreKey() = runBlocking {
    keys.keyOrNew()
    val prover = ProviderProver(keys.proofKey)
    val transport = Script(
      TokenEndpointAnswer(400u, """{"error":"use_dpop_nonce"}""".toByteArray(), "provider-nonce", null),
      TokenEndpointAnswer(200u, "{}".toByteArray(), null, null),
    )
    val answer = prover.tokenRequest(ENDPOINT, TokenProofFallback.NONE, transport)
    assertEquals(200, answer.status.toInt())
    assertEquals(2, transport.sent.size)
    val public = publicKey(keys.proofKey.publicKey())
    for (proof in transport.sent) verify(proof!!, public)
    val second = JSONObject(String(decode(transport.sent[1]!!.split('.')[1])))
    assertEquals("provider-nonce", second.getString("nonce"))
    assertEquals("POST", second.getString("htm"))
    assertEquals(ENDPOINT, second.getString("htu"))
    assertEquals(prover.thumbprint(), ProviderProver(keys.proofKey).thumbprint())
  }

  @Test
  fun theKeyIsKeptAndADeletedKeyIsLost() {
    val first = keys.keyOrNew().publicPoint
    assertTrue(first.contentEquals(keys.keyOrNew().publicPoint))
    keys.delete()
    try {
      keys.proofKey.publicKey()
      fail("a deleted key must be lost")
    } catch (expected: DeviceProofException.KeyLost) {
    }
    assertTrue(!first.contentEquals(keys.keyOrNew().publicPoint))
  }

  private class Script(vararg answers: TokenEndpointAnswer) : TokenEndpointTransport {
    private val answers = answers.toMutableList()
    val sent = mutableListOf<String?>()

    override suspend fun send(proof: String?): TokenEndpointAnswer {
      sent += proof
      return answers.removeAt(0)
    }
  }

  /** The key [point] names, on P-256 (the curve's parameters taken from a throwaway key). */
  private fun publicKey(point: ByteArray): ECPublicKey {
    val params = (
      KeyPairGenerator.getInstance("EC").apply { initialize(ECGenParameterSpec("secp256r1")) }
        .generateKeyPair().public as ECPublicKey
      ).params
    val w = ECPoint(BigInteger(1, point.copyOfRange(1, 33)), BigInteger(1, point.copyOfRange(33, 65)))
    return KeyFactory.getInstance("EC").generatePublic(ECPublicKeySpec(w, params)) as ECPublicKey
  }

  /** The JWS signature, `r || s`, checked with [public]. */
  private fun verify(jws: String, public: ECPublicKey) {
    val parts = jws.split('.')
    val verifier = Signature.getInstance("SHA256withECDSA")
    verifier.initVerify(public)
    verifier.update("${parts[0]}.${parts[1]}".toByteArray())
    assertTrue("the proof verifies with the Keystore key", verifier.verify(der(decode(parts[2]))))
  }

  private fun decode(part: String): ByteArray = Base64.decode(part, Base64.URL_SAFE or Base64.NO_PADDING or Base64.NO_WRAP)

  /** `r || s` as DER, for the platform's verifier. */
  private fun der(raw: ByteArray): ByteArray {
    fun integer(bytes: ByteArray): ByteArray {
      val value = BigInteger(1, bytes).toByteArray()
      return byteArrayOf(0x02, value.size.toByte()) + value
    }
    val body = integer(raw.copyOfRange(0, 32)) + integer(raw.copyOfRange(32, 64))
    return byteArrayOf(0x30, body.size.toByte()) + body
  }
}
