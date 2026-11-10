//! A tiny stand-in for the parts of Keycloak's admin API the OIDC sync uses.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use secrecy::SecretString;
use serde_json::{Value, json};
use teamcraft_server::config::RoleNames;
use teamcraft_server::state::OidcSyncSettings;
use teamcraft_server::testing::TEST_ISSUER;

pub const CLIENT_SECRET: &str = "fake-client-secret";
const ADMIN_TOKEN: &str = "fake-admin-token";
const CLIENT_UUID: &str = "client-uuid";

#[derive(Debug, Clone)]
pub struct FakeUser {
  pub id: String,
  pub is_enabled: bool,
  pub roles: Vec<String>,
}

/// A group of the fake provider: its members are user IDs.
#[derive(Debug, Clone)]
pub struct FakeGroup {
  pub id: String,
  pub name: String,
  pub members: Vec<String>,
}

pub struct FakeKeycloak {
  pub base_url: String,
  state: Arc<Mutex<Vec<FakeUser>>>,
  groups: Arc<Mutex<Vec<FakeGroup>>>,
  faults: Faults,
}

/// What the fake is told to refuse.
#[derive(Clone, Default)]
struct Faults {
  /// A user whose role lookup answers garbage.
  maybe_broken_roles: Arc<Mutex<Option<String>>>,
  refuses_token: Arc<std::sync::atomic::AtomicBool>,
}

type SharedGroups = Arc<Mutex<Vec<FakeGroup>>>;

type Shared = Arc<Mutex<Vec<FakeUser>>>;

impl FakeKeycloak {
  pub async fn start(users: Vec<FakeUser>) -> Self {
    let state: Shared = Arc::new(Mutex::new(users));
    let groups: SharedGroups = Arc::new(Mutex::new(Vec::new()));
    let faults = Faults::default();
    let app = Router::new()
      .route(
        "/realms/test/protocol/openid-connect/token",
        post(token).with_state(faults.clone()),
      )
      .route("/admin/realms/test/clients", get(clients))
      .route("/admin/realms/test/users", get(list_users))
      .route(
        "/admin/realms/test/users/{id}/role-mappings/clients/{client}/composite",
        get(roles).with_state((state.clone(), faults.clone())),
      )
      .route(
        "/admin/realms/test/groups",
        get(list_groups).with_state(groups.clone()),
      )
      .route(
        "/admin/realms/test/groups/{id}/members",
        get(group_members).with_state(groups.clone()),
      )
      .route(
        "/admin/realms/test/group-by-path/{*path}",
        get(group_by_path).with_state(groups.clone()),
      )
      .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
      axum::serve(listener, app).await.unwrap();
    });
    Self {
      base_url,
      state,
      groups,
      faults,
    }
  }

  /// The role lookup of `id` answers something that is no role list.
  pub fn break_roles_of(&self, id: &str) {
    *self.faults.maybe_broken_roles.lock().unwrap() = Some(id.to_owned());
  }

  pub fn mend_roles(&self) {
    *self.faults.maybe_broken_roles.lock().unwrap() = None;
  }

  /// The token endpoint refuses the service account from now on.
  pub fn refuse_token(&self) {
    self
      .faults
      .refuses_token
      .store(true, std::sync::atomic::Ordering::Relaxed);
  }

  pub fn remove_all(&self) {
    self.state.lock().unwrap().clear();
  }

  /// Settings that point the sync at this fake.
  pub fn sync_settings(&self, client_secret: &str, page_size: u32) -> OidcSyncSettings {
    OidcSyncSettings {
      issuer: TEST_ISSUER.into(),
      token_url: format!("{}/realms/test/protocol/openid-connect/token", self.base_url),
      admin_url: format!("{}/admin/realms/test", self.base_url),
      client_id: "teamcraft".into(),
      server_client: "teamcraft-server".into(),
      client_secret: SecretString::from(client_secret.to_owned()),
      roles: RoleNames::default(),
      page_size,
      sync_groups: false,
      mode: teamcraft_server::config::OidcSyncRunMode::Apply,
      brake: teamcraft_server::services::oidc_brake::Brake::default(),
      // Tests that sign people in by hand compare whole reports; those about pre-creation turn it on.
      precreation: teamcraft_server::services::oidc::AccountPrecreation::Off,
      interval: std::time::Duration::from_secs(300),
      stale_after_intervals: 3,
    }
  }

  pub fn update(&self, id: &str, edit: impl FnOnce(&mut FakeUser)) {
    let mut users = self.state.lock().unwrap();
    edit(users.iter_mut().find(|user| user.id == id).unwrap());
  }

  /// Replaces the provider's groups.
  pub fn set_groups(&self, groups: Vec<FakeGroup>) {
    *self.groups.lock().unwrap() = groups;
  }

  pub fn remove(&self, id: &str) {
    self.state.lock().unwrap().retain(|user| user.id != id);
  }
}

pub fn fake_user(subject: &str, roles: &[&str]) -> FakeUser {
  FakeUser {
    id: subject.to_owned(),
    is_enabled: true,
    roles: roles.iter().map(|r| (*r).to_owned()).collect(),
  }
}

async fn token(
  State(faults): State<Faults>,
  Form(form): Form<HashMap<String, String>>,
) -> Result<Json<Value>, StatusCode> {
  if faults.refuses_token.load(std::sync::atomic::Ordering::Relaxed) {
    return Err(StatusCode::UNAUTHORIZED);
  }
  let valid = form.get("grant_type").map(String::as_str) == Some("client_credentials")
    && form.get("client_id").map(String::as_str) == Some("teamcraft-server")
    && form.get("client_secret").map(String::as_str) == Some(CLIENT_SECRET);
  if valid {
    Ok(Json(json!({"access_token": ADMIN_TOKEN})))
  } else {
    Err(StatusCode::UNAUTHORIZED)
  }
}

fn authorized(headers: &HeaderMap) -> Result<(), StatusCode> {
  let expected = format!("Bearer {ADMIN_TOKEN}");
  if headers.get("authorization").and_then(|v| v.to_str().ok()) == Some(expected.as_str()) {
    Ok(())
  } else {
    Err(StatusCode::UNAUTHORIZED)
  }
}

async fn clients(
  headers: HeaderMap,
  Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, StatusCode> {
  authorized(&headers)?;
  if query.get("clientId").map(String::as_str) == Some("teamcraft") {
    Ok(Json(json!([{"id": CLIENT_UUID}])))
  } else {
    Ok(Json(json!([])))
  }
}

async fn list_users(
  State(state): State<Shared>,
  headers: HeaderMap,
  Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, StatusCode> {
  authorized(&headers)?;
  let first: usize = query.get("first").and_then(|v| v.parse().ok()).unwrap_or(0);
  let max: usize = query.get("max").and_then(|v| v.parse().ok()).unwrap_or(100);
  let users = state.lock().unwrap();
  // `username` (with `exact=true`) finds one user; the fake's user names are their IDs.
  let maybe_username = query.get("username");
  let page: Vec<Value> = users
    .iter()
    .filter(|user| maybe_username.is_none_or(|name| *name == user.id))
    .skip(first)
    .take(max)
    .map(|user| json!({"id": user.id, "username": user.id, "enabled": user.is_enabled}))
    .collect();
  Ok(Json(Value::Array(page)))
}

async fn roles(
  State((state, faults)): State<(Shared, Faults)>,
  headers: HeaderMap,
  Path((id, client)): Path<(String, String)>,
) -> Result<Json<Value>, StatusCode> {
  authorized(&headers)?;
  if faults.maybe_broken_roles.lock().unwrap().as_deref() == Some(id.as_str()) {
    return Ok(Json(json!({"not": "a list"})));
  }
  if client != CLIENT_UUID {
    return Err(StatusCode::NOT_FOUND);
  }
  let users = state.lock().unwrap();
  let user = users
    .iter()
    .find(|user| user.id == id)
    .ok_or(StatusCode::NOT_FOUND)?;
  Ok(Json(Value::Array(
    user.roles.iter().map(|name| json!({"name": name})).collect(),
  )))
}

pub fn fake_group(id: &str, name: &str, members: &[&str]) -> FakeGroup {
  FakeGroup {
    id: id.to_owned(),
    name: name.to_owned(),
    members: members.iter().map(|m| (*m).to_owned()).collect(),
  }
}

async fn list_groups(
  State(groups): State<SharedGroups>,
  headers: HeaderMap,
  Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, StatusCode> {
  authorized(&headers)?;
  let first: usize = query.get("first").and_then(|v| v.parse().ok()).unwrap_or(0);
  let max: usize = query.get("max").and_then(|v| v.parse().ok()).unwrap_or(100);
  let page: Vec<Value> = groups
    .lock()
    .unwrap()
    .iter()
    .skip(first)
    .take(max)
    .map(|group| json!({"id": group.id, "name": group.name, "path": format!("/{}", group.name)}))
    .collect();
  Ok(Json(Value::Array(page)))
}

async fn group_by_path(
  State(groups): State<SharedGroups>,
  headers: HeaderMap,
  Path(path): Path<String>,
) -> Result<Json<Value>, StatusCode> {
  authorized(&headers)?;
  let groups = groups.lock().unwrap();
  let group = groups
    .iter()
    .find(|group| group.name == path.trim_matches('/'))
    .ok_or(StatusCode::NOT_FOUND)?;
  Ok(Json(
    json!({"id": group.id, "name": group.name, "path": format!("/{}", group.name)}),
  ))
}

async fn group_members(
  State(groups): State<SharedGroups>,
  headers: HeaderMap,
  Path(id): Path<String>,
  Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, StatusCode> {
  authorized(&headers)?;
  let first: usize = query.get("first").and_then(|v| v.parse().ok()).unwrap_or(0);
  let max: usize = query.get("max").and_then(|v| v.parse().ok()).unwrap_or(100);
  let groups = groups.lock().unwrap();
  let group = groups
    .iter()
    .find(|group| group.id == id)
    .ok_or(StatusCode::NOT_FOUND)?;
  let page: Vec<Value> = group
    .members
    .iter()
    .skip(first)
    .take(max)
    .map(|member| json!({"id": member, "enabled": true}))
    .collect();
  Ok(Json(Value::Array(page)))
}
