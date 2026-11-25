package de.zeitgleich.teamcraft.security

import android.os.Build
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyPermanentlyInvalidatedException
import android.security.keystore.KeyProperties
import android.util.Log
import de.zeitgleich.teamcraft.core.UiThreadGuard
import java.io.IOException
import java.math.BigInteger
import java.nio.ByteBuffer
import java.security.GeneralSecurityException
import java.security.KeyFactory
import java.security.KeyPairGenerator
import java.security.KeyStore
import java.security.PrivateKey
import java.security.Signature
import java.security.UnrecoverableKeyException
import java.security.interfaces.ECPublicKey
import java.security.spec.ECGenParameterSpec
import java.security.spec.PKCS8EncodedKeySpec
import java.security.spec.X509EncodedKeySpec
import uniffi.teamcraft_ffi.DeviceProofException
import uniffi.teamcraft_ffi.DeviceProofKey

/**
 * The device's key for DPoP proofs (docs/e2ee.md §3.1.2, issue #120), one per server, gone with its
 * session at sign-out. The app keeps the key and signs bytes; what a proof says, nonces, the clock and
 * retries are the core's ([DeviceProofKey], `ProviderProver`), as on iOS.
 *
 * Read anew for each use, so the core never signs with a key a sign-out removed. A key that can't be
 * read keeps the session (a wait, like a network problem); only a store that was read and has no key,
 * or a key the Keystore says is gone, is a lost key: signing in again is the way out.
 */
class DeviceKeyStore(private val vault: DeviceKeyVault, private val log: (String) -> Unit = ::logWarning) {
  /** What the core and the token requests sign with. */
  val proofKey: DeviceProofKey = VaultProofKey(vault, log)

  /**
   * The key, made on first use: in the Keystore's hardware where it makes one, otherwise in
   * software. A store that can't be read throws, so that it can't replace a key that is there; a
   * key the Keystore says is gone is replaced.
   */
  fun keyOrNew(): StoredDeviceKey = synchronized(this) {
    val maybeExisting = try {
      vault.read()
    } catch (gone: DeviceKeyException.Gone) {
      vault.delete()
      null
    }
    maybeExisting ?: newKey()
  }

  /** Sign-out: the next sign-in makes a new key. */
  fun delete() = synchronized(this) { vault.delete() }

  private fun newKey(): StoredDeviceKey =
    try {
      vault.createInHardware()
    } catch (refused: Exception) {
      // Only the type: nothing else is known to be free of what the Keystore was asked.
      log("the Keystore made no device key (${refused.javaClass.simpleName}), the device key is a software key")
      vault.delete()
      vault.createInSoftware()
    }
}

/** Where a server's device key is kept; [KeystoreDeviceKeyVault] on the phone. */
interface DeviceKeyVault {
  /** The key; `null` when the store was read and has none. Throws [DeviceKeyException]. */
  fun read(): StoredDeviceKey?

  /** A new key in the Keystore's hardware; throws when the Keystore refuses. */
  fun createInHardware(): StoredDeviceKey

  /** A new software key, sealed in the app's files. */
  fun createInSoftware(): StoredDeviceKey

  /** Nothing to do about a failure: the next sign-in replaces the key or reuses it. */
  fun delete()
}

/** A key as the vault holds it. */
interface StoredDeviceKey {
  /** The public half as an uncompressed SEC1 point (`0x04`, x, y). */
  val publicPoint: ByteArray

  /** The ES256 signature of [message]: 64 bytes, `r || s`. Throws [DeviceKeyException]. */
  fun sign(message: ByteArray): ByteArray
}

/** Why a key can't be used. No key material in here. */
sealed class DeviceKeyException(message: String, cause: Throwable?) : Exception(message, cause) {
  /** The store was read and the key is gone, or the Keystore says it can't be used again. */
  class Gone(cause: Throwable? = null) : DeviceKeyException("the device key is gone", cause)

  /** The store doesn't answer right now; nothing is decided about the key. */
  class Unreadable(cause: Throwable? = null) : DeviceKeyException("the device key can't be read now", cause)
}

/**
 * The vault's key as the core signs with it, with the errors the core tells apart. Called by the core
 * through UniFFI: anything else thrown here than [DeviceProofException] is no answer the core can
 * use, so every failure is mapped, whatever the Keystore throws (#2393).
 */
class VaultProofKey(
  private val vault: DeviceKeyVault,
  private val log: (String) -> Unit = ::logWarning,
) : DeviceProofKey {
  override fun publicKey(): ByteArray = answered { stored().publicPoint }

  override fun sign(message: ByteArray): ByteArray = answered { stored().sign(message) }

  private fun stored(): StoredDeviceKey = vault.read() ?: throw DeviceProofException.KeyLost()

  /**
   * [work]'s result, or the error the core tells apart: a key the store says is gone is lost, any
   * other failure decides nothing about the session (a wait, or the session stays unbound).
   */
  private inline fun <T> answered(work: () -> T): T =
    try {
      work()
    } catch (answer: DeviceProofException) {
      throw answer
    } catch (gone: DeviceKeyException.Gone) {
      throw DeviceProofException.KeyLost()
    } catch (fatal: VirtualMachineError) {
      throw fatal
    } catch (failure: Throwable) {
      // Only the type: nothing else is known to be free of what the Keystore was asked.
      log("the device key failed (${failure.javaClass.simpleName}), nothing is decided")
      throw DeviceProofException.KeyUnavailable()
    }
}

/**
 * A server's key in the Android Keystore under [alias]: a non-exportable P-256 key, in StrongBox
 * where the phone has one, else in the TEE. It signs without asking the user, also while the phone
 * is locked, as push wake-ups need. Only where the Keystore refuses to make one, a software key
 * takes its place, sealed in [softwareFile] with a Keystore key ([KeystoreBox]). Neither reaches
 * another device: a restored phone has no key, and no session either.
 */
class KeystoreDeviceKeyVault(private val alias: String, private val softwareFile: SealedFile) : DeviceKeyVault {
  override fun read(): StoredDeviceKey? {
    UiThreadGuard.shared.check("KeystoreDeviceKeyVault.read")
    val entry = try {
      keyStore().getEntry(alias, null)
    } catch (failure: Exception) {
      throw classified(failure)
    }
    if (entry is KeyStore.PrivateKeyEntry) return HardwareKey(entry)
    return readSoftware()
  }

  override fun createInHardware(): StoredDeviceKey {
    UiThreadGuard.shared.check("KeystoreDeviceKeyVault.createInHardware")
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
      try {
        return generate(isStrongBox = true)
      } catch (unavailable: Exception) {
        // Most phones have no StrongBox (StrongBoxUnavailableException), and some have one that
        // makes no EC key: the TEE-backed key below is the normal case.
        deleteEntry()
      }
    }
    return generate(isStrongBox = false)
  }

  override fun createInSoftware(): StoredDeviceKey {
    UiThreadGuard.shared.check("KeystoreDeviceKeyVault.createInSoftware")
    val pair = KeyPairGenerator.getInstance(KeyProperties.KEY_ALGORITHM_EC)
      .apply { initialize(ECGenParameterSpec(CURVE)) }
      .generateKeyPair()
    softwareFile.write(SoftwareKeyRecord.encode(pair.public.encoded, pair.private.encoded))
    return SoftwareKey(pair.public as ECPublicKey, pair.private)
  }

  override fun delete() {
    UiThreadGuard.shared.check("KeystoreDeviceKeyVault.delete")
    deleteEntry()
    softwareFile.delete()
  }

  private fun generate(isStrongBox: Boolean): StoredDeviceKey {
    val spec = KeyGenParameterSpec.Builder(alias, KeyProperties.PURPOSE_SIGN)
      .setAlgorithmParameterSpec(ECGenParameterSpec(CURVE))
      .setDigests(KeyProperties.DIGEST_SHA256)
      .apply {
        if (isStrongBox && Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) setIsStrongBoxBacked(true)
      }
      .build()
    KeyPairGenerator.getInstance(KeyProperties.KEY_ALGORITHM_EC, ANDROID_KEYSTORE)
      .apply { initialize(spec) }
      .generateKeyPair()
    val entry = keyStore().getEntry(alias, null) as? KeyStore.PrivateKeyEntry
      ?: throw DeviceKeyException.Unreadable()
    return HardwareKey(entry)
  }

  private fun readSoftware(): StoredDeviceKey? {
    val record = try {
      softwareFile.read() ?: return null
    } catch (failure: IOException) {
      throw DeviceKeyException.Unreadable(failure)
    } catch (failure: Exception) {
      // Sealed under a Keystore key that is gone, or damaged: this key can't come back.
      throw DeviceKeyException.Gone(failure)
    }
    return try {
      val (public, private) = SoftwareKeyRecord.decode(record)
      val factory = KeyFactory.getInstance(KeyProperties.KEY_ALGORITHM_EC)
      SoftwareKey(
        factory.generatePublic(X509EncodedKeySpec(public)) as ECPublicKey,
        factory.generatePrivate(PKCS8EncodedKeySpec(private)),
      )
    } catch (damaged: Exception) {
      throw DeviceKeyException.Gone(damaged)
    } finally {
      record.fill(0)
    }
  }

  private fun deleteEntry() {
    try {
      keyStore().deleteEntry(alias)
    } catch (failure: Exception) {
      // Nothing to do: the next sign-in makes a new key under the same alias, replacing this one.
      Log.w(TAG, "device key not removed: ${failure.javaClass.simpleName}")
    }
  }

  /** A key in the Keystore. Its private half never leaves it. */
  private class HardwareKey(private val entry: KeyStore.PrivateKeyEntry) : StoredDeviceKey {
    override val publicPoint: ByteArray = Es256.uncompressedPoint(entry.certificate.publicKey as ECPublicKey)

    override fun sign(message: ByteArray): ByteArray =
      try {
        Es256.sign(entry.privateKey, message)
      } catch (failure: Exception) {
        throw classified(failure)
      }
  }

  private class SoftwareKey(public: ECPublicKey, private val private: PrivateKey) : StoredDeviceKey {
    override val publicPoint: ByteArray = Es256.uncompressedPoint(public)

    override fun sign(message: ByteArray): ByteArray =
      try {
        Es256.sign(private, message)
      } catch (failure: GeneralSecurityException) {
        throw DeviceKeyException.Unreadable(failure)
      }
  }

  private companion object {
    const val ANDROID_KEYSTORE = "AndroidKeyStore"
    const val CURVE = "secp256r1"
    const val TAG = "DeviceKey"

    fun keyStore(): KeyStore =
      try {
        KeyStore.getInstance(ANDROID_KEYSTORE).apply { load(null) }
      } catch (failure: Exception) {
        throw DeviceKeyException.Unreadable(failure)
      }

    /**
     * What a Keystore failure says about the key. Gone: the key was invalidated, can't be unwrapped
     * any more, or the Keystore (Android 13+) says it doesn't exist or is corrupted. Anything else
     * may pass (the Keystore busy or restarting) and decides nothing.
     */
    fun classified(failure: Exception): DeviceKeyException {
      if (failure is DeviceKeyException) return failure
      if (failure is KeyPermanentlyInvalidatedException || failure is UnrecoverableKeyException) {
        return DeviceKeyException.Gone(failure)
      }
      if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
        val maybeKeystoreFailure = generateSequence<Throwable>(failure) { it.cause }
          .filterIsInstance<android.security.KeyStoreException>()
          .firstOrNull()
        if (maybeKeystoreFailure != null &&
          !maybeKeystoreFailure.isTransientFailure &&
          maybeKeystoreFailure.numericErrorCode in KEY_GONE_ERRORS
        ) {
          return DeviceKeyException.Gone(failure)
        }
      }
      return DeviceKeyException.Unreadable(failure)
    }

    val KEY_GONE_ERRORS: Set<Int>
      get() = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
        setOf(
          android.security.KeyStoreException.ERROR_KEY_DOES_NOT_EXIST,
          android.security.KeyStoreException.ERROR_KEY_CORRUPTED,
        )
      } else {
        emptySet()
      }
  }
}

/** ES256 as a JWS carries it (RFC 7518 §3.4), from what the platform's EC classes give. */
object Es256 {
  private const val COORDINATE_BYTES = 32
  private const val SEQUENCE: Byte = 0x30
  private const val INTEGER: Byte = 0x02

  /** The signature of [message] with [key]: SHA-256 with ECDSA, as `r || s`. */
  fun sign(key: PrivateKey, message: ByteArray): ByteArray {
    val signer = Signature.getInstance("SHA256withECDSA")
    signer.initSign(key)
    signer.update(message)
    return rawSignature(signer.sign())
  }

  /** [key] as an uncompressed SEC1 point: `0x04`, x, y, each coordinate 32 bytes. */
  fun uncompressedPoint(key: ECPublicKey): ByteArray =
    byteArrayOf(0x04) + fixedLength(key.w.affineX) + fixedLength(key.w.affineY)

  /** A DER `ECDSA-Sig-Value` (SEQUENCE of r and s) as `r || s`, 32 bytes each. */
  fun rawSignature(der: ByteArray): ByteArray {
    val input = ByteBuffer.wrap(der)
    require(input.get() == SEQUENCE) { "not a DER sequence" }
    val length = readLength(input)
    require(length == input.remaining()) { "DER sequence length" }
    val r = readInteger(input)
    val s = readInteger(input)
    require(!input.hasRemaining()) { "trailing bytes after the signature" }
    return fixedLength(r) + fixedLength(s)
  }

  private fun readInteger(input: ByteBuffer): BigInteger {
    require(input.get() == INTEGER) { "not a DER integer" }
    val length = readLength(input)
    require(length in 1..input.remaining()) { "DER integer length" }
    val bytes = ByteArray(length).also { input.get(it) }
    val value = BigInteger(bytes)
    require(value.signum() > 0) { "a signature value is positive" }
    return value
  }

  /** Short form, or the long form with one length byte: an ES256 signature is at most 72 bytes. */
  private fun readLength(input: ByteBuffer): Int {
    val first = input.get().toInt() and 0xff
    if (first < 0x80) return first
    require(first == 0x81) { "DER length" }
    return input.get().toInt() and 0xff
  }

  private fun fixedLength(value: BigInteger): ByteArray {
    val bytes = value.toByteArray()
    // toByteArray adds a sign byte for a value with its top bit set.
    val magnitude = if (bytes.size > COORDINATE_BYTES && bytes[0] == 0.toByte()) bytes.copyOfRange(1, bytes.size) else bytes
    require(magnitude.size <= COORDINATE_BYTES) { "a P-256 value has at most 32 bytes" }
    return ByteArray(COORDINATE_BYTES - magnitude.size) + magnitude
  }
}

/** A software key as sealed: version, the public key's length (2 bytes), X.509 public key, PKCS #8 private key. */
internal object SoftwareKeyRecord {
  private const val VERSION: Byte = 1

  fun encode(public: ByteArray, private: ByteArray): ByteArray {
    require(public.size <= 0xffff)
    return byteArrayOf(VERSION, (public.size shr 8).toByte(), public.size.toByte()) + public + private
  }

  fun decode(record: ByteArray): Pair<ByteArray, ByteArray> {
    require(record.size > 3 && record[0] == VERSION) { "not a device key record" }
    val publicLength = ((record[1].toInt() and 0xff) shl 8) or (record[2].toInt() and 0xff)
    require(record.size > 3 + publicLength) { "device key record length" }
    return record.copyOfRange(3, 3 + publicLength) to record.copyOfRange(3 + publicLength, record.size)
  }
}

private fun logWarning(message: String) {
  Log.w("DeviceKey", message)
}
