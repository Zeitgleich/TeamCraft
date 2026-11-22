package de.zeitgleich.teamcraft.core

import androidx.annotation.StringRes
import de.zeitgleich.teamcraft.R
import uniffi.teamcraft_ffi.CommandFailure
import uniffi.teamcraft_ffi.CommandFailureKind

/**
 * The text of a queued action the server or the network refused (#2242), by the server's error code:
 * the same wording as the web client's `failure_message_key`. Unknown codes read as a generic failure.
 */
@StringRes
fun commandFailureText(failure: CommandFailure): Int = when (failure.kind) {
  CommandFailureKind.UNREACHABLE -> R.string.error_network
  CommandFailureKind.OTHER -> R.string.error_generic
  CommandFailureKind.REFUSED -> when (failure.maybeErrorCode) {
    "legal_hold" -> R.string.error_legal_hold
    "last_channel_admin" -> R.string.error_last_channel_admin
    "import_in_progress" -> R.string.error_import_in_progress
    "space_has_hidden_channels" -> R.string.error_space_has_hidden_channels
    "forbidden", "not_space_member" -> R.string.error_forbidden
    "unauthorized", "invalid_token", "account_deleted" -> R.string.error_signed_out
    "rate_limited" -> R.string.error_rate_limited
    "conflict", "epoch_conflict", "version_conflict" -> R.string.error_conflict
    "payload_too_large" -> R.string.error_too_large
    "unavailable", "internal" -> R.string.error_server_trouble
    // Every `*_not_found` code, including ones this version doesn't know yet.
    else -> when {
      failure.maybeErrorCode?.endsWith("_not_found") == true -> R.string.error_not_found
      else -> R.string.error_generic
    }
  }
}
