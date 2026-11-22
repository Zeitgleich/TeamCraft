//! The dev stack's Keycloak, configured for the browser tests through its admin API: every web
//! test port is a redirect target and CORS origin of the apps' client, and a copy of that client
//! hands out short-lived tokens for tests that wait for a token to run out. Both are set on the
//! running realm, so a Keycloak imported before a port or the copy existed needs no new import.

use serde_json::{Value, json};
use tokio::sync::OnceCell;

use crate::stack::KEYCLOAK_URL;

/// The dev realm and the apps' client in it (dev/keycloak/teamcraft-dev-realm.json).
pub const REALM: &str = "teamcraft-dev";
pub const APP_CLIENT: &str = "teamcraft";

/// The dev stack's admin account (docker-compose.dev.yml); dev only.
const ADMIN_USER: &str = "admin";
const ADMIN_PASSWORD: &str = "admin-dev-only";

/// The dev realm's admin API.
pub struct KeycloakAdmin {
  http: reqwest::Client,
}

impl KeycloakAdmin {
  pub fn new() -> Self {
    Self {
      http: reqwest::Client::new(),
    }
  }

  /// The client's internal ID (`clients/<uuid>`).
  pub async fn client_uuid(&self, client_id: &str) -> Result<Option<String>, String> {
    let clients = self.get(&format!("/clients?clientId={client_id}")).await?;
    Ok(clients[0]["id"].as_str().map(str::to_owned))
  }

  pub async fn get(&self, path: &str) -> Result<Value, String> {
    let answer = self
      .http
      .get(format!("{}{path}", admin_url()))
      .bearer_auth(self.token().await?)
      .send()
      .await
      .map_err(|error| format!("GET {path}: {error}"))?;
    if !answer.status().is_success() {
      return Err(format!("GET {path}: {}", answer.status()));
    }
    answer
      .json()
      .await
      .map_err(|error| format!("GET {path}: {error}"))
  }

  pub async fn send(&self, method: reqwest::Method, path: &str, body: &Value) -> Result<(), String> {
    let answer = self
      .http
      .request(method.clone(), format!("{}{path}", admin_url()))
      .bearer_auth(self.token().await?)
      .json(body)
      .send()
      .await
      .map_err(|error| format!("{method} {path}: {error}"))?;
    if answer.status().is_success() {
      Ok(())
    } else {
      Err(format!("{method} {path}: {}", answer.status()))
    }
  }

  /// A new token for every call: the admin API's lives a minute, and tests take longer.
  async fn token(&self) -> Result<String, String> {
    let answer: Value = self
      .http
      .post(format!(
        "{KEYCLOAK_URL}/realms/master/protocol/openid-connect/token"
      ))
      .form(&[
        ("grant_type", "password"),
        ("client_id", "admin-cli"),
        ("username", ADMIN_USER),
        ("password", ADMIN_PASSWORD),
      ])
      .send()
      .await
      .map_err(|error| format!("the admin token: {error}"))?
      .json()
      .await
      .map_err(|error| format!("the admin token: {error}"))?;
    answer["access_token"]
      .as_str()
      .map(str::to_owned)
      .ok_or_else(|| format!("no admin token: {answer}"))
  }
}

impl Default for KeycloakAdmin {
  fn default() -> Self {
    Self::new()
  }
}

/// Makes every port of `ports` a redirect target (`http://127.0.0.1:<port>/*`) and CORS origin of
/// the apps' client, once per test process. Writers race only with the same additions.
pub async fn ensure_web_ports(ports: &[u16]) -> Result<(), String> {
  static DONE: OnceCell<()> = OnceCell::const_new();
  DONE
    .get_or_try_init(|| async {
      let admin = KeycloakAdmin::new();
      let uuid = admin
        .client_uuid(APP_CLIENT)
        .await?
        .ok_or("the apps' client is missing")?;
      let path = format!("/clients/{uuid}");
      let mut client = admin.get(&path).await?;
      let is_changed = add_web_ports(&mut client, ports);
      if is_changed {
        admin.send(reqwest::Method::PUT, &path, &client).await?;
      }
      Ok(())
    })
    .await
    .map(|_| ())
}

/// Adds what is missing of `ports` to the client's redirect URIs and web origins; whether anything was.
fn add_web_ports(client: &mut Value, ports: &[u16]) -> bool {
  let mut is_changed = false;
  for (field, suffix) in [("redirectUris", "/*"), ("webOrigins", "")] {
    let mut list = client[field].as_array().cloned().unwrap_or_default();
    for port in ports {
      let wanted = Value::String(format!("http://127.0.0.1:{port}{suffix}"));
      if !list.contains(&wanted) {
        list.push(wanted);
        is_changed = true;
      }
    }
    client[field] = Value::Array(list);
  }
  is_changed
}

/// A copy of the apps' client named `client_id` whose access tokens live `lifespan_secs`, with the
/// same roles (owner, admin, member, guest), scopes and mappers; its role mapper names the copy.
/// Already there, its lifespan and the original's redirect URIs and web origins are set again.
pub async fn ensure_short_token_client(client_id: &str, lifespan_secs: u32) -> Result<String, String> {
  let admin = KeycloakAdmin::new();
  let lifespan = lifespan_secs.to_string();
  let original_uuid = admin
    .client_uuid(APP_CLIENT)
    .await?
    .ok_or("the apps' client is missing")?;
  let original = admin.get(&format!("/clients/{original_uuid}")).await?;
  if let Some(uuid) = admin.client_uuid(client_id).await? {
    let path = format!("/clients/{uuid}");
    let mut client = admin.get(&path).await?;
    client["attributes"]["access.token.lifespan"] = json!(lifespan);
    client["redirectUris"] = original["redirectUris"].clone();
    client["webOrigins"] = original["webOrigins"].clone();
    admin.send(reqwest::Method::PUT, &path, &client).await?;
    return Ok(uuid);
  }
  let copy = short_token_copy(&original, client_id, &lifespan);
  admin.send(reqwest::Method::POST, "/clients", &copy).await?;
  let uuid = admin
    .client_uuid(client_id)
    .await?
    .ok_or("the copied client is missing")?;
  let roles = admin.get(&format!("/clients/{original_uuid}/roles")).await?;
  for role in roles.as_array().cloned().unwrap_or_default() {
    let role = json!({ "name": role["name"], "description": role["description"] });
    admin
      .send(reqwest::Method::POST, &format!("/clients/{uuid}/roles"), &role)
      .await?;
  }
  Ok(uuid)
}

/// The original client's representation as a new client: no IDs, its own name and token lifespan,
/// and mappers of client roles pointed at the copy.
fn short_token_copy(original: &Value, client_id: &str, lifespan: &str) -> Value {
  let mut copy = original.clone();
  let Some(fields) = copy.as_object_mut() else {
    return copy;
  };
  fields.remove("id");
  fields.insert("clientId".into(), json!(client_id));
  fields.insert(
    "name".into(),
    json!(format!("TeamCraft apps, {lifespan} s tokens (tests)")),
  );
  copy["attributes"]["access.token.lifespan"] = json!(lifespan);
  if let Some(mappers) = copy["protocolMappers"].as_array_mut() {
    for mapper in mappers.iter_mut() {
      if let Some(fields) = mapper.as_object_mut() {
        fields.remove("id");
      }
      if mapper["config"]["usermodel.clientRoleMapping.clientId"] == APP_CLIENT {
        mapper["config"]["usermodel.clientRoleMapping.clientId"] = json!(client_id);
      }
    }
  }
  copy
}

fn admin_url() -> String {
  format!("{KEYCLOAK_URL}/admin/realms/{REALM}")
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn missing_ports_are_added_once() {
    let mut client = json!({
      "redirectUris": ["http://127.0.0.1:8083/*"],
      "webOrigins": ["http://127.0.0.1:8083"],
    });
    assert!(add_web_ports(&mut client, &[8083, 8084]));
    assert_eq!(
      client["redirectUris"],
      json!(["http://127.0.0.1:8083/*", "http://127.0.0.1:8084/*"])
    );
    assert_eq!(
      client["webOrigins"],
      json!(["http://127.0.0.1:8083", "http://127.0.0.1:8084"])
    );
    assert!(!add_web_ports(&mut client, &[8083, 8084]));
  }

  #[test]
  fn the_copy_has_its_own_name_lifespan_and_role_mapper() {
    let original = json!({
      "id": "abc",
      "clientId": APP_CLIENT,
      "attributes": { "pkce.code.challenge.method": "S256" },
      "protocolMappers": [
        { "id": "m1", "name": "client roles in ID token",
          "config": { "usermodel.clientRoleMapping.clientId": APP_CLIENT } },
        { "id": "m2", "name": "groups", "config": {} },
      ],
    });
    let copy = short_token_copy(&original, "teamcraft-short", "60");
    assert!(copy.get("id").is_none());
    assert_eq!(copy["clientId"], "teamcraft-short");
    assert_eq!(copy["attributes"]["access.token.lifespan"], "60");
    assert_eq!(copy["attributes"]["pkce.code.challenge.method"], "S256");
    assert!(copy["protocolMappers"][0].get("id").is_none());
    assert_eq!(
      copy["protocolMappers"][0]["config"]["usermodel.clientRoleMapping.clientId"],
      "teamcraft-short"
    );
  }
}
