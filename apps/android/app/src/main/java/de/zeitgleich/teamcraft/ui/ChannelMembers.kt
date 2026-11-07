package de.zeitgleich.teamcraft.ui

import androidx.annotation.StringRes
import de.zeitgleich.teamcraft.R
import uniffi.teamcraft_ffi.ChannelMemberRole
import uniffi.teamcraft_ffi.ReviewedRolesOutcome
import uniffi.teamcraft_ffi.RoleKeyMarkItem
import uniffi.teamcraft_ffi.RoleReviewRefusalItem

/*
 * A channel's members section (issue #1262), as on desktop and web: what each mark says. What is
 * marked and refused is the core's rule (`teamcraft_client_api::role_review`); the app only words it.
 */

/** The badge of a role; a member, or a role this version doesn't know, has none. */
@StringRes
fun channelRoleLabel(role: ChannelMemberRole): Int? = when (role) {
  ChannelMemberRole.ADMIN -> R.string.channel_role_admin
  ChannelMemberRole.MODERATOR -> R.string.channel_role_moderator
  ChannelMemberRole.MEMBER, ChannelMemberRole.OTHER -> null
}

/** The mark on a reviewed row's key; a checked key has none. */
@StringRes
fun roleKeyMarkLabel(mark: RoleKeyMarkItem): Int? = when (mark) {
  RoleKeyMarkItem.CHECKED -> null
  RoleKeyMarkItem.CHANGED -> R.string.channel_roles_review_key_changed
  RoleKeyMarkItem.UNCHECKABLE -> R.string.channel_roles_review_key_uncheckable
}

/** Why the list can't be confirmed. */
@StringRes
fun roleReviewRefusalText(refusal: RoleReviewRefusalItem): Int = when (refusal) {
  RoleReviewRefusalItem.UNKNOWN_PERSON -> R.string.channel_roles_review_unknown_refused
  RoleReviewRefusalItem.UNCHECKABLE_KEY -> R.string.channel_roles_review_uncheckable_refused
  RoleReviewRefusalItem.NO_ADMIN -> R.string.channel_roles_review_no_admin
}

/** What confirming says afterwards; `null` once it is done (the panel then shows the new roles). */
@StringRes
fun reviewedRolesMessage(outcome: ReviewedRolesOutcome): Int? = when (outcome) {
  ReviewedRolesOutcome.CONFIRMED -> null
  ReviewedRolesOutcome.CHANGED -> R.string.channel_roles_review_changed
  ReviewedRolesOutcome.REFUSED -> R.string.error_generic
}

/** Who confirming takes a role from, by name; somebody nobody can name is [unknown]. */
fun droppedRoleNames(dropped: List<String?>, unknown: String): String =
  dropped.joinToString(", ") { it ?: unknown }
