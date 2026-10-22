use axum::Router;
use axum::routing::{delete, get, patch, post, put};
use tower_http::trace::TraceLayer;

use crate::handlers::{
  add_conversation_member_handler, add_space_member_handler, admin_deactivate_user_handler,
  admin_list_audit_handler, admin_list_conversation_members_handler, admin_list_conversations_handler,
  admin_list_space_members_handler, admin_list_spaces_handler, admin_list_user_devices_handler,
  admin_list_users_handler, admin_reactivate_user_handler, admin_revoke_device_handler,
  append_policy_handler, append_roster_handler, backchannel_logout_handler, claim_key_packages_handler,
  complete_attachment_handler, create_attachment_handler, create_call_join_token_handler,
  create_category_handler, create_channel_handler, create_dm_handler, create_group_dm_handler,
  create_invite_handler, create_space_handler, delete_category_handler, delete_invite_handler,
  download_attachment_handler, gateway_handler, get_conversation_handler, get_conversation_retention_handler,
  get_identity_key_handler, get_key_bag_handler, get_me_handler, get_policy_handler,
  get_security_log_handler, get_space_handler, get_space_retention_handler, get_user_handler,
  healthz_handler, join_conversation_handler, join_space_handler, key_package_status_handler,
  list_categories_handler, list_conversation_members_handler, list_conversations_handler,
  list_devices_handler, list_invites_handler, list_messages_handler, list_mls_messages_handler,
  list_policy_log_handler, list_presence_handler, list_roster_handler, list_space_members_handler,
  list_spaces_handler, list_user_devices_handler, list_welcomes_handler, pin_message_handler,
  post_mls_commit_handler, post_mls_message_handler, put_identity_key_handler, put_key_bag_handler,
  readyz_handler, register_device_handler, register_push_handler, remove_conversation_member_handler,
  remove_space_member_handler, rendezvous_handler, revoke_device_handler, search_users_handler,
  send_message_handler, set_conversation_retention_handler, set_presence_handler, set_read_position_handler,
  set_space_retention_handler, unpin_message_handler, unregister_push_handler, update_category_handler,
  update_conversation_handler, update_conversation_member_handler, update_space_handler,
  update_space_member_handler, upload_key_packages_handler,
};
use crate::openapi::openapi_json_handler;
use crate::state::AppState;

pub fn router(state: AppState) -> Router {
  let api_v1 = Router::new()
    .route("/me", get(get_me_handler))
    .route("/gateway", get(gateway_handler))
    .route("/rendezvous", get(rendezvous_handler))
    .route("/auth/backchannel-logout", post(backchannel_logout_handler))
    .route("/openapi.json", get(openapi_json_handler))
    .route(
      "/devices",
      post(register_device_handler).get(list_devices_handler),
    )
    .route("/devices/{device_id}", delete(revoke_device_handler))
    .route("/users", get(search_users_handler))
    .route("/users/{user_id}", get(get_user_handler))
    .route("/spaces", get(list_spaces_handler).post(create_space_handler))
    .route(
      "/spaces/{space_id}",
      get(get_space_handler).patch(update_space_handler),
    )
    .route("/spaces/{space_id}/join", post(join_space_handler))
    .route(
      "/spaces/{space_id}/members",
      get(list_space_members_handler).post(add_space_member_handler),
    )
    .route(
      "/spaces/{space_id}/members/{user_id}",
      patch(update_space_member_handler).delete(remove_space_member_handler),
    )
    .route(
      "/spaces/{space_id}/categories",
      get(list_categories_handler).post(create_category_handler),
    )
    .route(
      "/spaces/{space_id}/categories/{category_id}",
      patch(update_category_handler).delete(delete_category_handler),
    )
    .route(
      "/spaces/{space_id}/invites",
      post(create_invite_handler).get(list_invites_handler),
    )
    .route(
      "/spaces/{space_id}/invites/{invite_id}",
      delete(delete_invite_handler),
    )
    .route("/conversations", get(list_conversations_handler))
    .route(
      "/conversations/{conversation_id}",
      get(get_conversation_handler).patch(update_conversation_handler),
    )
    .route("/spaces/{space_id}/channels", post(create_channel_handler))
    .route("/dms", post(create_dm_handler))
    .route("/group-dms", post(create_group_dm_handler))
    .route(
      "/conversations/{conversation_id}/members",
      get(list_conversation_members_handler).post(add_conversation_member_handler),
    )
    .route(
      "/conversations/{conversation_id}/members/{user_id}",
      patch(update_conversation_member_handler).delete(remove_conversation_member_handler),
    )
    .route(
      "/conversations/{conversation_id}/join",
      post(join_conversation_handler),
    )
    .route(
      "/conversations/{conversation_id}/call/join-token",
      post(create_call_join_token_handler),
    )
    .route(
      "/conversations/{conversation_id}/read-position",
      put(set_read_position_handler),
    )
    .route(
      "/conversations/{conversation_id}/messages",
      post(send_message_handler).get(list_messages_handler),
    )
    .route("/me/presence", put(set_presence_handler))
    .route("/presence", get(list_presence_handler))
    .route("/me/identity-key", put(put_identity_key_handler))
    .route("/users/{user_id}/identity-key", get(get_identity_key_handler))
    .route("/users/{user_id}/devices", get(list_user_devices_handler))
    .route(
      "/devices/{device_id}/key-packages",
      post(upload_key_packages_handler).get(key_package_status_handler),
    )
    .route(
      "/users/{user_id}/key-packages/claim",
      post(claim_key_packages_handler),
    )
    .route(
      "/conversations/{conversation_id}/mls/commits",
      post(post_mls_commit_handler),
    )
    .route(
      "/conversations/{conversation_id}/mls/messages",
      post(post_mls_message_handler),
    )
    .route(
      "/conversations/{conversation_id}/mls",
      get(list_mls_messages_handler),
    )
    .route("/devices/{device_id}/welcomes", get(list_welcomes_handler))
    .route("/me/key-bag", put(put_key_bag_handler).get(get_key_bag_handler))
    .route(
      "/devices/{device_id}/push",
      put(register_push_handler).delete(unregister_push_handler),
    )
    .route("/attachments", post(create_attachment_handler))
    .route(
      "/attachments/{attachment_id}/complete",
      post(complete_attachment_handler),
    )
    .route(
      "/attachments/{attachment_id}/download",
      get(download_attachment_handler),
    )
    .route(
      "/workspace/roster",
      post(append_roster_handler).get(list_roster_handler),
    )
    .route(
      "/workspace/policy",
      post(append_policy_handler).get(get_policy_handler),
    )
    .route("/workspace/policy/log", get(list_policy_log_handler))
    .route("/admin/spaces", get(admin_list_spaces_handler))
    .route(
      "/admin/spaces/{space_id}/members",
      get(admin_list_space_members_handler),
    )
    .route("/admin/conversations", get(admin_list_conversations_handler))
    .route(
      "/admin/conversations/{conversation_id}/members",
      get(admin_list_conversation_members_handler),
    )
    .route("/admin/users", get(admin_list_users_handler))
    .route(
      "/admin/users/{user_id}/devices",
      get(admin_list_user_devices_handler),
    )
    .route(
      "/admin/users/{user_id}/deactivate",
      post(admin_deactivate_user_handler),
    )
    .route(
      "/admin/users/{user_id}/reactivate",
      post(admin_reactivate_user_handler),
    )
    .route(
      "/admin/users/{user_id}/devices/{device_id}",
      delete(admin_revoke_device_handler),
    )
    .route("/admin/audit", get(admin_list_audit_handler))
    .route("/me/security-log", get(get_security_log_handler))
    .route(
      "/spaces/{space_id}/retention",
      get(get_space_retention_handler).put(set_space_retention_handler),
    )
    .route(
      "/conversations/{conversation_id}/retention",
      get(get_conversation_retention_handler).put(set_conversation_retention_handler),
    )
    .route(
      "/conversations/{conversation_id}/messages/{message_id}/pin",
      put(pin_message_handler).delete(unpin_message_handler),
    );

  Router::new()
    .route("/healthz", get(healthz_handler))
    .route("/readyz", get(readyz_handler))
    .nest("/api/v1", api_v1)
    .layer(TraceLayer::new_for_http())
    .with_state(state)
}
