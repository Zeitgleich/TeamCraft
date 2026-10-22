use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::wire_enum::wire_enum;
use crate::{Base64Bytes, DeviceId, UnixMillis, UserId};

wire_enum! {
  /// Employee or external person (ARCHITECTURE.md §4.1).
  pub enum UserKind {
    Member => "member",
    Guest => "guest",
  }
}

/// What other users see of a user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UserSummary {
  pub id: UserId,
  #[serde(rename = "username", default, skip_serializing_if = "Option::is_none")]
  pub maybe_username: Option<String>,
  pub display_name: String,
  pub kind: UserKind,
}

/// `GET /api/v1/users?query=` – the directory. Guests only find people they share a conversation with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SearchUsersResponse {
  pub users: Vec<UserSummary>,
}

/// `POST /api/v1/devices`. Keys are opaque to the server until the crypto milestone verifies them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RegisterDeviceRequest {
  pub name: String,
  pub public_key: Base64Bytes,
  pub cross_signature: Base64Bytes,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DeviceResponse {
  pub id: DeviceId,
  pub name: String,
  pub public_key: Base64Bytes,
  /// The identity key's signature over the device key; clients verify it, the server doesn't.
  pub cross_signature: Base64Bytes,
  /// How the device receives pushes, if registered (the token and key are never returned).
  #[serde(rename = "push_provider", default, skip_serializing_if = "Option::is_none")]
  pub maybe_push_provider: Option<PushProvider>,
  pub created_at: UnixMillis,
}

wire_enum! {
  /// Push service of a device (e2ee.md §7).
  pub enum PushProvider {
    Apns => "apns",
    Fcm => "fcm",
    UnifiedPush => "unifiedpush",
    /// Browser Web Push (VAPID); the token is the JSON subscription.
    WebPush => "webpush",
  }
}

/// `PUT /api/v1/devices/{id}/push`: registers (or replaces) the device's push channel. The push key
/// is a random 256-bit key shared with the server so push payloads are opaque to Apple/Google.
/// Sending pushes is not part of this endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RegisterPushRequest {
  pub provider: PushProvider,
  pub token: String,
  pub push_key: Base64Bytes,
}

/// `PUT /api/v1/devices/{id}/certificate`: replaces the device's cross-signature once the final device
/// certificate is known. The certificate signs the device ID, which the server assigns, so a client
/// registers with a provisional certificate and uploads the final one right after.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SetDeviceCertificateRequest {
  pub cross_signature: Base64Bytes,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ListDevicesResponse {
  pub devices: Vec<DeviceResponse>,
}

/// Query of `GET /api/v1/users`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SearchUsersQuery {
  /// Matched against display name and username; empty lists the first people alphabetically.
  #[serde(default)]
  pub query: String,
}

/// `PUT /api/v1/me/identity-key`: publishes the user's public identity key (e2ee.md §3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UploadIdentityKeyRequest {
  pub public_key: Base64Bytes,
  /// Self-signature or proof of possession; opaque to the server.
  pub signature: Base64Bytes,
  /// The signed OIDC ID token carrying the key fingerprint as claim `tc_identity_key` (e2ee.md §3.1).
  /// Clients verify it against the identity provider; the server only stores it.
  #[serde(rename = "attestation", default, skip_serializing_if = "Option::is_none")]
  pub maybe_attestation: Option<String>,
  /// Replaces an existing key. Only a full reset does this; other clients see the change.
  #[serde(default)]
  pub replace: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct IdentityKeyResponse {
  pub user_id: UserId,
  pub public_key: Base64Bytes,
  pub signature: Base64Bytes,
  #[serde(rename = "attestation", default, skip_serializing_if = "Option::is_none")]
  pub maybe_attestation: Option<String>,
  pub created_at: UnixMillis,
}

wire_enum! {
  /// Why a device is woken up (e2ee.md §7).
  pub enum PushKind {
    /// A message in a conversation the user follows.
    Message => "message",
    /// A message that mentions the user, or arrives in a DM.
    Mention => "mention",
  }
}

/// The plaintext of a push payload. The server seals it with the device's push key
/// (XChaCha20-Poly1305), so Apple, Google and push relays see only ciphertext. The app (or the iOS
/// notification service extension) opens it, fetches the real message and decrypts that locally.
/// It names the conversation and message but carries no content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PushWakeUp {
  pub kind: PushKind,
  pub conversation_id: crate::ConversationId,
  #[serde(rename = "message_id", default, skip_serializing_if = "Option::is_none")]
  pub maybe_message_id: Option<crate::MessageId>,
  #[serde(rename = "sender_id", default, skip_serializing_if = "Option::is_none")]
  pub maybe_sender_id: Option<UserId>,
  /// The sender marked the message urgent.
  #[serde(default)]
  pub is_urgent: bool,
}
