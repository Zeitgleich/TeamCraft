package de.zeitgleich.teamcraft.auth

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.SerializationException
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonPrimitive

private val json = Json { ignoreUnknownKeys = true }

/** Treat the token as expired this long before the server does, so no request races the expiry. */
private const val EXPIRY_MARGIN_MILLIS = 30_000L

/**
 * The session of a local account (e-mail and password, `POST /api/v1/auth/local/login`): the
 * server's bearer token for the API and the gateway, its expiry and the account. There is no
 * refresh; once it expired the user signs in again. Kept sealed per server (`AuthRepository`).
 */
@Serializable
data class LocalSession(
  @SerialName("access_token") val accessToken: String,
  /** Unix milliseconds, by the server's clock. */
  @SerialName("expires_at") val expiresAtMillis: Long,
  @SerialName("user_id") val userId: String,
  /**
   * The server's clock minus the device's at sign-in (from the answer's `Date`), so a device
   * clock that is off doesn't end the session early or keep it past its end.
   */
  @SerialName("clock_offset") val clockOffsetMillis: Long = 0,
) {
  /** Whether the token is still good at [deviceNowMillis] (the device's clock). */
  fun isValid(deviceNowMillis: Long): Boolean = deviceNowMillis + clockOffsetMillis < expiresAtMillis - EXPIRY_MARGIN_MILLIS

  fun encode(): ByteArray = json.encodeToString(serializer(), this).encodeToByteArray()

  // The token never reaches a log through a `toString()`.
  override fun toString(): String = "LocalSession([redacted])"

  companion object {
    /** `null` for unreadable data: the user signs in again. */
    fun decode(bytes: ByteArray): LocalSession? =
      try {
        json.decodeFromString(serializer(), bytes.decodeToString()).takeIf { it.accessToken.isNotEmpty() && it.userId.isNotEmpty() }
      } catch (unreadable: SerializationException) {
        null
      } catch (unreadable: IllegalArgumentException) {
        null
      }
  }
}

/** What the server answered to a local sign-in. */
sealed interface LocalLoginOutcome {
  data class SignedIn(val session: LocalSession) : LocalLoginOutcome

  /**
   * Wrong e-mail, password or code (or a code missing): one answer for all, so nobody learns
   * which accounts exist or whether a password was right.
   */
  data object Rejected : LocalLoginOutcome

  /** The server doesn't offer local accounts (switched off, or not set up). */
  data object Disabled : LocalLoginOutcome

  /** Too many attempts; try again later. */
  data object RateLimited : LocalLoginOutcome

  /** The server is there but can't answer now. */
  data object Unavailable : LocalLoginOutcome

  /** A redirect or an HTML page: the network wants a sign-in first (captive portal). */
  data object Intercepted : LocalLoginOutcome

  /** Anything else the app doesn't understand. */
  data object Failed : LocalLoginOutcome
}

/**
 * Reads the answer to `POST /api/v1/auth/local/login`. [maybeServerDateMillis] is the answer's
 * `Date` header; [deviceNowMillis] the device's clock when it arrived.
 */
fun localLoginOutcome(status: Int, body: ByteArray, maybeServerDateMillis: Long?, deviceNowMillis: Long): LocalLoginOutcome {
  if (status == 200) return signedIn(body, maybeServerDateMillis, deviceNowMillis) ?: LocalLoginOutcome.Intercepted
  if (status in 300..399) return LocalLoginOutcome.Intercepted
  return when (errorCode(body)) {
    // The server answers a missing code with "unauthorized"; "mfa_required" is treated the same.
    "unauthorized", "invalid_request", "mfa_required" -> LocalLoginOutcome.Rejected
    "local_accounts_disabled" -> LocalLoginOutcome.Disabled
    "rate_limited" -> LocalLoginOutcome.RateLimited
    else -> when (status) {
      401, 400 -> LocalLoginOutcome.Rejected
      403 -> LocalLoginOutcome.Disabled
      429 -> LocalLoginOutcome.RateLimited
      // 503 without an error code we know: local accounts are not set up on this server.
      503 -> LocalLoginOutcome.Disabled
      in 500..599 -> LocalLoginOutcome.Unavailable
      else -> LocalLoginOutcome.Failed
    }
  }
}

private fun signedIn(body: ByteArray, maybeServerDateMillis: Long?, deviceNowMillis: Long): LocalLoginOutcome.SignedIn? {
  val response = try {
    json.decodeFromString(LocalLoginResponse.serializer(), body.decodeToString())
  } catch (unreadable: SerializationException) {
    return null
  } catch (unreadable: IllegalArgumentException) {
    return null
  }
  if (response.accessToken.isEmpty() || response.userId.isEmpty()) return null
  val offset = maybeServerDateMillis?.let { it - deviceNowMillis } ?: 0L
  return LocalLoginOutcome.SignedIn(LocalSession(response.accessToken, response.expiresAtMillis, response.userId, offset))
}

private fun errorCode(body: ByteArray): String? =
  try {
    (json.parseToJsonElement(body.decodeToString()) as? JsonObject)?.get("error_code")?.jsonPrimitive?.contentOrNull
  } catch (unreadable: SerializationException) {
    null
  } catch (unreadable: IllegalArgumentException) {
    null
  }

@Serializable
private class LocalLoginResponse(
  @SerialName("access_token") val accessToken: String,
  @SerialName("expires_at") val expiresAtMillis: Long,
  @SerialName("user_id") val userId: String,
)
