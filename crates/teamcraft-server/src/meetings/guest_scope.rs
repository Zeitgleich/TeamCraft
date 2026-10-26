//! What a meeting guest's session token may call (docs/meetings.md, "Guests in the browser").
//!
//! A guest has no account at the identity provider and is a member of nothing but the one meeting
//! chat, so the data they can reach is small already. This list is the second wall: a route that is
//! not named here answers 403 for a guest session, whatever the route's own checks say, so a new
//! endpoint is closed to guests until somebody decides it should be open.

use axum::http::Method;

/// The prefix every route of the API is nested under.
const API_PREFIX: &str = "/api/v1";

/// Routes a guest may read (`GET`, `HEAD`).
const READ_ONLY: &[&str] = &[
  "/gateway",
  "/me",
  "/unread",
  "/threads",
  "/spaces",
  "/workspace/roster",
  "/workspace/policy",
  "/workspace/policy/log",
  "/me/notification-settings",
  "/me/privacy",
  // Answered for the people the guest shares a conversation with only (services/presence.rs).
  "/presence",
  "/org-backgrounds/{background_id}/download",
  "/me/backgrounds/{background_id}/download",
  "/conversations",
  "/conversations/{conversation_id}",
  "/conversations/{conversation_id}/members",
  "/conversations/{conversation_id}/system-events",
  "/conversations/{conversation_id}/call",
  "/conversations/{conversation_id}/mls",
  "/attachments/{attachment_id}/download",
  "/users/{user_id}",
  "/users/{user_id}/identity-key",
  "/users/{user_id}/enrollment",
  "/users/{user_id}/device-list",
  "/users/{user_id}/devices",
  "/meetings/{meeting_id}",
  "/meetings/{meeting_id}/lobby/me",
];

/// Routes a guest may read and change: the chat of their meeting and the key material of their own
/// device (the routes themselves check that it is theirs).
const READ_WRITE: &[&str] = &[
  "/me/identity-key",
  "/me/key-bag",
  "/me/device-list",
  "/devices",
  "/devices/{device_id}",
  "/devices/{device_id}/certificate",
  "/devices/{device_id}/key-packages",
  "/devices/{device_id}/welcomes",
  "/conversations/{conversation_id}/messages",
  "/conversations/{conversation_id}/messages/{message_id}/acknowledgement",
  "/me/presence",
  "/conversations/{conversation_id}/read-position",
  "/conversations/{conversation_id}/threads/{thread_id}/read",
  "/conversations/{conversation_id}/read-receipts",
  "/conversations/{conversation_id}/mls/commits",
  "/conversations/{conversation_id}/mls/messages",
  "/conversations/{conversation_id}/mls/group-info",
  "/conversations/{conversation_id}/call/join-token",
  // Only ever takes the guest's own devices out of the call.
  "/conversations/{conversation_id}/call/leave",
  "/meetings/{meeting_id}/leave",
  "/meetings/{meeting_id}/lobby/me/proof",
];

/// Whether a guest session may call `method` on the route `matched_path` (axum's route pattern, as
/// registered, including the `/api/v1` prefix). Unmatched requests (no route) are not a guest's.
pub fn allows(method: &Method, maybe_matched_path: Option<&str>) -> bool {
  let Some(path) = maybe_matched_path.and_then(|path| path.strip_prefix(API_PREFIX)) else {
    return false;
  };
  if READ_WRITE.contains(&path) {
    return true;
  }
  matches!(*method, Method::GET | Method::HEAD) && READ_ONLY.contains(&path)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn guests_reach_their_chat_and_device() {
    assert!(allows(
      &Method::POST,
      Some("/api/v1/conversations/{conversation_id}/messages")
    ));
    assert!(allows(&Method::GET, Some("/api/v1/devices/{device_id}/welcomes")));
    assert!(allows(&Method::POST, Some("/api/v1/meetings/{meeting_id}/leave")));
  }

  #[test]
  fn guests_only_read_where_the_route_is_read_only() {
    assert!(allows(
      &Method::GET,
      Some("/api/v1/conversations/{conversation_id}")
    ));
    assert!(!allows(
      &Method::PATCH,
      Some("/api/v1/conversations/{conversation_id}")
    ));
    assert!(!allows(&Method::POST, Some("/api/v1/conversations")));
  }

  #[test]
  fn everything_else_is_closed() {
    for path in [
      "/api/v1/users",
      "/api/v1/dms",
      "/api/v1/meetings",
      "/api/v1/meetings/instant",
      "/api/v1/meetings/{meeting_id}/lobby/{user_id}/admit",
      "/api/v1/spaces/{space_id}/join",
      "/api/v1/admin/users",
      "/api/v1/devices/{device_id}/push",
      "/api/v1/attachments",
    ] {
      assert!(!allows(&Method::GET, Some(path)), "{path}");
      assert!(!allows(&Method::POST, Some(path)), "{path}");
    }
    assert!(!allows(&Method::GET, None));
    assert!(!allows(&Method::GET, Some("/api/internal/v1/meetings/replies")));
  }
}
