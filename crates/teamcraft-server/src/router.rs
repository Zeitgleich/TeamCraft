use axum::Router;
use axum::routing::{delete, get, patch, post, put};
use tower_http::trace::TraceLayer;

use crate::handlers::{
  add_conversation_member_handler, add_space_member_handler, create_category_handler, create_channel_handler,
  create_dm_handler, create_group_dm_handler, create_invite_handler, create_space_handler,
  delete_category_handler, delete_invite_handler, gateway_handler, get_conversation_handler, get_me_handler,
  get_space_handler, get_user_handler, healthz_handler, join_conversation_handler, join_space_handler,
  list_categories_handler, list_conversation_members_handler, list_conversations_handler,
  list_devices_handler, list_invites_handler, list_messages_handler, list_presence_handler,
  list_space_members_handler, list_spaces_handler, readyz_handler, register_device_handler,
  remove_conversation_member_handler, remove_space_member_handler, revoke_device_handler,
  search_users_handler, send_message_handler, set_presence_handler, set_read_position_handler,
  update_category_handler, update_conversation_handler, update_conversation_member_handler,
  update_space_handler, update_space_member_handler,
};
use crate::openapi::openapi_json_handler;
use crate::state::AppState;

pub fn router(state: AppState) -> Router {
  let api_v1 = Router::new()
    .route("/me", get(get_me_handler))
    .route("/gateway", get(gateway_handler))
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
      "/conversations/{conversation_id}/read-position",
      put(set_read_position_handler),
    )
    .route(
      "/conversations/{conversation_id}/messages",
      post(send_message_handler).get(list_messages_handler),
    )
    .route("/me/presence", put(set_presence_handler))
    .route("/presence", get(list_presence_handler));

  Router::new()
    .route("/healthz", get(healthz_handler))
    .route("/readyz", get(readyz_handler))
    .nest("/api/v1", api_v1)
    .layer(TraceLayer::new_for_http())
    .with_state(state)
}
