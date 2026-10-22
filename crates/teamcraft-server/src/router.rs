use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::middleware;
use axum::routing::{delete, get, patch, post, put};
use tower_http::trace::TraceLayer;

use crate::handlers::{
  acknowledge_message_handler, add_conversation_member_handler, add_space_member_handler,
  add_user_group_member_handler, admin_assign_channel_owner_handler, admin_clear_announcement_handler,
  admin_create_bot_handler, admin_deactivate_user_handler, admin_get_channel_moderation_handler,
  admin_get_permission_scheme_handler, admin_get_space_permission_scheme_handler, admin_instance_handler,
  admin_list_audit_handler, admin_list_bots_handler, admin_list_conversation_members_handler,
  admin_list_conversations_handler, admin_list_space_members_handler, admin_list_spaces_handler,
  admin_list_user_devices_handler, admin_list_users_handler, admin_reactivate_user_handler,
  admin_reissue_local_activation_handler, admin_revoke_bot_handler, admin_revoke_device_handler,
  admin_rotate_bot_token_handler, admin_set_announcement_handler, admin_set_channel_moderation_handler,
  admin_update_permission_scheme_handler, admin_update_space_permission_scheme_handler, admit_guest_handler,
  append_policy_handler, append_roster_handler, approve_recovery_request_handler, backchannel_logout_handler,
  cancel_meeting_handler, cancel_recovery_request_handler, change_local_password_handler,
  claim_key_packages_handler, clear_custom_status_handler, complete_attachment_handler,
  complete_avatar_upload_handler, control_meeting_handler, create_attachment_handler,
  create_avatar_upload_handler, create_call_join_token_handler, create_category_handler,
  create_channel_handler, create_dm_handler, create_group_dm_handler, create_guest_link_handler,
  create_instant_meeting_handler, create_invite_handler, create_local_guest_handler, create_meeting_handler,
  create_recovery_request_handler, create_share_link_handler, create_space_handler,
  create_user_group_handler, delete_category_handler, delete_invite_handler, delete_my_avatar_handler,
  delete_user_group_handler, deny_guest_handler, deny_recovery_request_handler, dismiss_announcement_handler,
  download_attachment_handler, download_share_handler, follow_thread_handler, gateway_handler,
  get_announcement_handler, get_call_handler, get_conversation_handler,
  get_conversation_notification_settings_handler, get_conversation_retention_handler,
  get_identity_key_handler, get_key_bag_handler, get_me_handler, get_meeting_handler,
  get_my_lobby_state_handler, get_my_org_recovery_shares_handler, get_my_profile_handler,
  get_notification_settings_handler, get_policy_handler, get_read_receipts_handler,
  get_recovery_material_handler, get_recovery_request_handler, get_recovery_result_handler,
  get_security_log_handler, get_share_info_handler, get_space_handler, get_space_retention_handler,
  get_unread_handler, get_user_avatar_handler, get_user_device_list_handler, get_user_group_handler,
  get_user_handler, get_user_profile_handler, get_web_push_key_handler, healthz_handler,
  join_conversation_handler, join_meeting_handler, join_space_handler, key_package_status_handler,
  list_acknowledgements_handler, list_call_history_handler, list_categories_handler,
  list_conversation_members_handler, list_conversations_handler, list_devices_handler, list_invites_handler,
  list_lobby_handler, list_meetings_handler, list_messages_handler, list_mls_messages_handler,
  list_policy_log_handler, list_presence_handler, list_recovery_requests_handler, list_roster_handler,
  list_share_links_handler, list_space_members_handler, list_spaces_handler, list_system_events_handler,
  list_threads_handler, list_user_devices_handler, list_user_groups_handler, list_welcomes_handler,
  local_activate_handler, local_login_handler, local_logout_handler, pin_message_handler,
  post_mls_commit_handler, post_mls_message_handler, put_conversation_notification_settings_handler,
  put_identity_key_handler, put_key_bag_handler, put_my_device_list_handler, put_my_profile_handler,
  put_notification_settings_handler, put_org_recovery_shares_handler, put_recovery_result_handler,
  put_recovery_share_handler, readyz_handler, receive_meeting_reply_handler, register_device_handler,
  register_push_handler, remove_conversation_member_handler, remove_space_member_handler,
  remove_user_group_member_handler, rendezvous_handler, report_participant_joined_handler,
  report_participant_left_handler, revoke_device_handler, revoke_share_link_handler, search_users_handler,
  send_message_handler, set_conversation_retention_handler, set_custom_status_handler,
  set_device_certificate_handler, set_meeting_rsvp_handler, set_presence_handler, set_read_position_handler,
  set_space_retention_handler, set_thread_read_handler, unfollow_thread_handler, unpin_message_handler,
  unregister_push_handler, update_category_handler, update_conversation_handler,
  update_conversation_member_handler, update_meeting_handler, update_space_handler,
  update_space_member_handler, update_user_group_handler, upload_key_packages_handler,
};
use crate::openapi::openapi_json_handler;
use crate::security;
use crate::state::AppState;
use crate::web;

pub fn router(state: AppState) -> Router {
  let api_v1 = Router::new()
    .route("/me", get(get_me_handler))
    .route("/push/web-key", get(get_web_push_key_handler))
    .route("/gateway", get(gateway_handler))
    .route("/rendezvous", get(rendezvous_handler))
    .route("/auth/backchannel-logout", post(backchannel_logout_handler))
    .route("/openapi.json", get(openapi_json_handler))
    .route("/meetings/join", post(join_meeting_handler))
    .route("/users/{user_id}/device-list", get(get_user_device_list_handler))
    .route("/me/device-list", put(put_my_device_list_handler))
    .route(
      "/shares",
      get(list_share_links_handler).post(create_share_link_handler),
    )
    .route(
      "/shares/{share_id}",
      get(get_share_info_handler).delete(revoke_share_link_handler),
    )
    .route("/shares/{share_id}/download", post(download_share_handler))
    .route("/org-recovery/shares", put(put_org_recovery_shares_handler))
    .route(
      "/org-recovery/shares/mine",
      get(get_my_org_recovery_shares_handler),
    )
    .route(
      "/org-recovery/requests",
      get(list_recovery_requests_handler).post(create_recovery_request_handler),
    )
    .route(
      "/org-recovery/requests/{request_id}",
      get(get_recovery_request_handler).delete(cancel_recovery_request_handler),
    )
    .route(
      "/org-recovery/requests/{request_id}/approve",
      post(approve_recovery_request_handler),
    )
    .route(
      "/org-recovery/requests/{request_id}/deny",
      post(deny_recovery_request_handler),
    )
    .route(
      "/org-recovery/requests/{request_id}/share",
      put(put_recovery_share_handler),
    )
    .route(
      "/org-recovery/requests/{request_id}/material",
      get(get_recovery_material_handler),
    )
    .route(
      "/org-recovery/requests/{request_id}/result",
      put(put_recovery_result_handler).get(get_recovery_result_handler),
    )
    .route("/calls/history", get(list_call_history_handler))
    .route("/meetings/{meeting_id}/controls", post(control_meeting_handler))
    .route("/announcement", get(get_announcement_handler))
    .route(
      "/announcement/{announcement_id}/dismiss",
      post(dismiss_announcement_handler),
    )
    .route(
      "/admin/announcement",
      put(admin_set_announcement_handler).delete(admin_clear_announcement_handler),
    )
    .route(
      "/conversations/{conversation_id}/system-events",
      get(list_system_events_handler),
    )
    .route(
      "/admin/permission-scheme",
      get(admin_get_permission_scheme_handler).put(admin_update_permission_scheme_handler),
    )
    .route(
      "/admin/spaces/{space_id}/permission-scheme",
      get(admin_get_space_permission_scheme_handler).put(admin_update_space_permission_scheme_handler),
    )
    .route(
      "/admin/conversations/{conversation_id}/moderation",
      get(admin_get_channel_moderation_handler).put(admin_set_channel_moderation_handler),
    )
    .route(
      "/admin/conversations/{conversation_id}/owner",
      post(admin_assign_channel_owner_handler),
    )
    .route(
      "/me/profile",
      get(get_my_profile_handler).put(put_my_profile_handler),
    )
    .route("/users/{user_id}/profile", get(get_user_profile_handler))
    .route(
      "/me/avatar",
      post(create_avatar_upload_handler).delete(delete_my_avatar_handler),
    )
    .route("/me/avatar/complete", post(complete_avatar_upload_handler))
    .route("/users/{user_id}/avatar", get(get_user_avatar_handler))
    .route(
      "/user-groups",
      get(list_user_groups_handler).post(create_user_group_handler),
    )
    .route(
      "/user-groups/{group_id}",
      get(get_user_group_handler)
        .patch(update_user_group_handler)
        .delete(delete_user_group_handler),
    )
    .route(
      "/user-groups/{group_id}/members/{user_id}",
      put(add_user_group_member_handler).delete(remove_user_group_member_handler),
    )
    .route("/auth/local/activate", post(local_activate_handler))
    .route("/auth/local/login", post(local_login_handler))
    .route("/auth/local/logout", post(local_logout_handler))
    .route("/auth/local/password", post(change_local_password_handler))
    .route(
      "/spaces/{space_id}/local-guests",
      post(create_local_guest_handler),
    )
    .route(
      "/admin/local-accounts/{user_id}/activation",
      post(admin_reissue_local_activation_handler),
    )
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
    .route("/admin/instance", get(admin_instance_handler))
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
    )
    .route(
      "/devices/{device_id}/certificate",
      put(set_device_certificate_handler),
    )
    .route("/conversations/{conversation_id}/call", get(get_call_handler))
    .route(
      "/conversations/{conversation_id}/call/join-token",
      post(create_call_join_token_handler),
    )
    .route(
      "/me/status",
      put(set_custom_status_handler).delete(clear_custom_status_handler),
    )
    .route("/threads", get(list_threads_handler))
    .route("/unread", get(get_unread_handler))
    .route(
      "/conversations/{conversation_id}/threads/{thread_id}/follow",
      put(follow_thread_handler).delete(unfollow_thread_handler),
    )
    .route(
      "/conversations/{conversation_id}/threads/{thread_id}/read",
      put(set_thread_read_handler),
    )
    .route(
      "/me/notification-settings",
      get(get_notification_settings_handler).put(put_notification_settings_handler),
    )
    .route(
      "/conversations/{conversation_id}/notification-settings",
      get(get_conversation_notification_settings_handler).put(put_conversation_notification_settings_handler),
    )
    .route(
      "/conversations/{conversation_id}/read-receipts",
      get(get_read_receipts_handler),
    )
    .route(
      "/conversations/{conversation_id}/messages/{message_id}/acknowledgement",
      put(acknowledge_message_handler),
    )
    .route(
      "/conversations/{conversation_id}/messages/{message_id}/acknowledgements",
      get(list_acknowledgements_handler),
    )
    .route(
      "/meetings",
      post(create_meeting_handler).get(list_meetings_handler),
    )
    .route("/meetings/instant", post(create_instant_meeting_handler))
    .route(
      "/meetings/{meeting_id}",
      get(get_meeting_handler)
        .patch(update_meeting_handler)
        .delete(cancel_meeting_handler),
    )
    .route("/meetings/{meeting_id}/rsvp", put(set_meeting_rsvp_handler))
    .route(
      "/meetings/{meeting_id}/guest-links",
      post(create_guest_link_handler),
    )
    .route("/meetings/{meeting_id}/lobby", get(list_lobby_handler))
    .route("/meetings/{meeting_id}/lobby/me", get(get_my_lobby_state_handler))
    .route(
      "/meetings/{meeting_id}/lobby/{user_id}/admit",
      post(admit_guest_handler),
    )
    .route(
      "/meetings/{meeting_id}/lobby/{user_id}/deny",
      post(deny_guest_handler),
    )
    .route(
      "/admin/bots",
      post(admin_create_bot_handler).get(admin_list_bots_handler),
    )
    .route("/admin/bots/{bot_id}/token", post(admin_rotate_bot_token_handler))
    .route("/admin/bots/{bot_id}", delete(admin_revoke_bot_handler));

  let internal = Router::new()
    .route(
      "/calls/participants/joined",
      post(report_participant_joined_handler),
    )
    .route("/calls/participants/left", post(report_participant_left_handler))
    .route("/meetings/replies", post(receive_meeting_reply_handler));

  Router::new()
    .route("/healthz", get(healthz_handler))
    .route("/readyz", get(readyz_handler))
    .nest("/api/v1", api_v1)
    .nest("/api/internal/v1", internal)
    .fallback_service(web::router(&state.settings.web))
    .layer(DefaultBodyLimit::max(
      state.settings.security.max_request_body_bytes,
    ))
    .layer(middleware::from_fn_with_state(state.clone(), security::guard))
    .layer(TraceLayer::new_for_http())
    .with_state(state)
}
