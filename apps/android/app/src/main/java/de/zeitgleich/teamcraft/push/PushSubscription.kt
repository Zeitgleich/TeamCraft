package de.zeitgleich.teamcraft.push

/**
 * The UnifiedPush registration token for the server: the same JSON as a browser's Web Push
 * subscription, so the server sends UnifiedPush like Web Push (RFC 8030/8291, optionally VAPID).
 */
object PushSubscription {
  fun json(endpoint: String, maybeP256dh: String?, maybeAuth: String?): String {
    val keys = if (maybeP256dh != null && maybeAuth != null) {
      ""","keys":{"p256dh":${quote(maybeP256dh)},"auth":${quote(maybeAuth)}}"""
    } else {
      ""
    }
    return """{"endpoint":${quote(endpoint)}$keys}"""
  }

  private fun quote(value: String): String =
    buildString {
      append('"')
      for (character in value) {
        when {
          character == '"' -> append("\\\"")
          character == '\\' -> append("\\\\")
          character < ' ' -> append("\\u%04x".format(character.code))
          else -> append(character)
        }
      }
      append('"')
    }
}
