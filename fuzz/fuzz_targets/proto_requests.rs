//! Every request body and query type of the REST API (`teamcraft-proto`) must decode without
//! panicking, and what decodes must encode and decode again: the server parses these from the
//! network. The first bytes are not special; libFuzzer mutates valid JSON seeds from the corpus.
#![no_main]

use libfuzzer_sys::fuzz_target;
use serde::{Serialize, de::DeserializeOwned};
use teamcraft_proto::*;

fn check<T: DeserializeOwned + Serialize>(data: &[u8]) {
  if let Ok(value) = serde_json::from_slice::<T>(data) {
    let encoded = serde_json::to_vec(&value).expect("a decoded value encodes");
    serde_json::from_slice::<T>(&encoded).expect("what we encode decodes again");
  }
}

fuzz_target!(|data: &[u8]| {
  check::<AddConversationMemberRequest>(data);
  check::<AddSpaceMemberRequest>(data);
  check::<AppendWorkspaceLogRequest>(data);
  check::<ClaimKeyPackagesRequest>(data);
  check::<CreateAttachmentRequest>(data);
  check::<CreateBotRequest>(data);
  check::<CreateCallJoinTokenRequest>(data);
  check::<CreateCategoryRequest>(data);
  check::<CreateChannelRequest>(data);
  check::<CreateDmRequest>(data);
  check::<CreateGroupDmRequest>(data);
  check::<CreateGuestLinkRequest>(data);
  check::<CreateInstantMeetingRequest>(data);
  check::<CreateInviteRequest>(data);
  check::<CreateMeetingRequest>(data);
  check::<CreateSpaceRequest>(data);
  check::<DeactivateUserRequest>(data);
  check::<JoinMeetingRequest>(data);
  check::<ListAdminConversationsQuery>(data);
  check::<ListAdminUsersQuery>(data);
  check::<ListAuditQuery>(data);
  check::<ListConversationsQuery>(data);
  check::<ListMeetingsQuery>(data);
  check::<ListMessagesQuery>(data);
  check::<ListMlsMessagesQuery>(data);
  check::<ListPresenceQuery>(data);
  check::<ListThreadsQuery>(data);
  check::<ListWorkspaceLogQuery>(data);
  check::<PostMlsCommitRequest>(data);
  check::<PostMlsMessageRequest>(data);
  check::<PutKeyBagRequest>(data);
  check::<RegisterDeviceRequest>(data);
  check::<RegisterPushRequest>(data);
  check::<SearchUsersQuery>(data);
  check::<SendMessageRequest>(data);
  check::<SetDeviceCertificateRequest>(data);
  check::<SetPresenceRequest>(data);
  check::<SetReadPositionRequest>(data);
  check::<SetRetentionRequest>(data);
  check::<SetRsvpRequest>(data);
  check::<SetThreadReadRequest>(data);
  check::<UpdateCategoryRequest>(data);
  check::<UpdateConversationMemberRequest>(data);
  check::<UpdateConversationRequest>(data);
  check::<UpdateMeetingRequest>(data);
  check::<UpdateSpaceMemberRequest>(data);
  check::<UpdateSpaceRequest>(data);
  check::<UploadIdentityKeyRequest>(data);
  check::<UploadKeyPackagesRequest>(data);
  check::<NotificationSettings>(data);
  check::<ConversationNotificationSettings>(data);
  check::<SfuParticipantReport>(data);
  check::<CustomStatus>(data);
  check::<WorkspaceLogEntryResponse>(data);
});
