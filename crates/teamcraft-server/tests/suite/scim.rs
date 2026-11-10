//! SCIM 2.0 provisioning (issue #1631, docs/provisioning.md §7) against Postgres: the token of
//! each client, every operation on users and groups, filters and paging, ETags, the brake, the
//! group mapping and retried requests. The request bodies are shaped like those Entra ID, Okta
//! and Keycloak's SCIM extensions send; none of them is called.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use common::*;
use reqwest::header::{HeaderMap, HeaderValue};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use teamcraft_server::testing::TestApp;

/// Entra ID's create: `userName` is the sign-in name, `externalId` its own mail nickname, the
/// enterprise extension along.
const ENTRA_CREATE: &str = r#"{
  "schemas": ["urn:ietf:params:scim:schemas:core:2.0:User", "urn:ietf:params:scim:schemas:extension:enterprise:2.0:User"],
  "externalId": "carol",
  "userName": "sub-carol",
  "active": true,
  "displayName": "Carol Example",
  "emails": [{"primary": true, "type": "work", "value": "carol@example.com"}],
  "meta": {"resourceType": "User"},
  "name": {"formatted": "Carol Example", "familyName": "Example", "givenName": "Carol"},
  "urn:ietf:params:scim:schemas:extension:enterprise:2.0:User": {"department": "Engineering", "manager": {"value": "x"}}
}"#;

/// Okta's create: `externalId` is not sent by every app template; the user name is the address.
const OKTA_CREATE: &str = r#"{
  "schemas": ["urn:ietf:params:scim:schemas:core:2.0:User"],
  "userName": "dave@example.com",
  "externalId": "sub-dave",
  "name": {"givenName": "Dave", "familyName": "Example"},
  "emails": [{"primary": true, "value": "dave@example.com", "type": "work"}],
  "displayName": "Dave Example",
  "locale": "en-US",
  "groups": [],
  "password": "never-stored",
  "active": true
}"#;

/// Keycloak SCIM extensions send PUT with the whole user, `externalId` being Keycloak's user ID.
const KEYCLOAK_CREATE: &str = r#"{
  "schemas": ["urn:ietf:params:scim:schemas:core:2.0:User"],
  "externalId": "sub-erin",
  "userName": "erin",
  "name": {"givenName": "Erin", "familyName": "Example"},
  "emails": [{"value": "erin@example.com"}],
  "active": true
}"#;

/// What a test drives the endpoint with: one client's token.
struct Scim<'a> {
  app: &'a TestApp,
  token: String,
}

struct Answer {
  status: StatusCode,
  body: Value,
  headers: HeaderMap,
}

impl Scim<'_> {
  async fn send(
    &self,
    method: Method,
    path: &str,
    maybe_body: Option<&str>,
    extra: &[(&str, &str)],
  ) -> Answer {
    send_as(self.app, Some(&self.token), method, path, maybe_body, extra).await
  }

  async fn get(&self, path: &str) -> Answer {
    self.send(Method::GET, path, None, &[]).await
  }

  async fn post(&self, path: &str, body: &str) -> Answer {
    self.send(Method::POST, path, Some(body), &[]).await
  }

  async fn put(&self, path: &str, body: &str) -> Answer {
    self.send(Method::PUT, path, Some(body), &[]).await
  }

  async fn patch(&self, path: &str, body: &Value) -> Answer {
    self.send(Method::PATCH, path, Some(&body.to_string()), &[]).await
  }

  async fn delete(&self, path: &str) -> Answer {
    self.send(Method::DELETE, path, None, &[]).await
  }

  /// Makes a user and returns its ID.
  async fn create_user(&self, body: &str) -> String {
    let answer = self.post("/Users", body).await;
    assert_eq!(answer.status, StatusCode::CREATED, "{}", answer.body);
    answer.body["id"].as_str().unwrap().to_owned()
  }
}

async fn send_as(
  app: &TestApp,
  maybe_token: Option<&str>,
  method: Method,
  path: &str,
  maybe_body: Option<&str>,
  extra: &[(&str, &str)],
) -> Answer {
  let mut request = app
    .http
    .request(method, format!("{}/scim/v2{path}", app.base_url));
  if let Some(token) = maybe_token {
    request = request.bearer_auth(token);
  }
  if let Some(body) = maybe_body {
    request = request
      .header("content-type", "application/scim+json")
      .body(body.to_owned());
  }
  for (name, value) in extra {
    request = request.header(*name, *value);
  }
  let response = request.send().await.unwrap();
  let status = response.status();
  let headers = response.headers().clone();
  let bytes = response.bytes().await.unwrap();
  Answer {
    status,
    body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    headers,
  }
}

async fn start() -> Option<TestApp> {
  start_with(|_| {}).await
}

async fn start_with(customize: impl FnOnce(&mut teamcraft_server::state::Settings)) -> Option<TestApp> {
  let app = TestApp::start_with(customize).await?;
  user_id(&app, &owner()).await;
  user_id(&app, &admin()).await;
  Some(app)
}

macro_rules! app {
  () => {
    match start().await {
      Some(app) => app,
      None => return,
    }
  };
  ($customize:expr) => {
    match start_with($customize).await {
      Some(app) => app,
      None => return,
    }
  };
}

/// A client whose identity attribute is `attribute`, and its token.
async fn client<'a>(app: &'a TestApp, attribute: &str) -> (Scim<'a>, String) {
  let reply = app
    .post(
      &admin(),
      "/admin/scim/clients",
      json!({"name": "Identity provider", "identity_attribute": attribute}),
    )
    .await;
  assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
  let token = reply.body["token"].as_str().unwrap().to_owned();
  let id = reply.body["client"]["id"].as_str().unwrap().to_owned();
  (Scim { app, token }, id)
}

mod auth {
  use super::*;

  #[tokio::test]
  async fn only_a_clients_own_token_is_accepted() {
    let app = app!();
    let (scim, client_id) = client(&app, "external_id").await;
    assert_eq!(scim.get("/Users").await.status, StatusCode::OK);

    let user_token = app.token(&admin());
    let wrong_secret = format!("tcscim_{client_id}_not-the-secret");
    for maybe_token in [
      None,
      Some(""),
      Some("garbage"),
      Some("tcscim_1_x"),
      Some(wrong_secret.as_str()),
      Some(user_token.as_str()),
    ] {
      let answer = send_as(&app, maybe_token, Method::GET, "/Users", None, &[]).await;
      assert_eq!(answer.status, StatusCode::UNAUTHORIZED, "{maybe_token:?}");
      assert_eq!(answer.body["error_code"], "invalid_token");
      assert_eq!(
        answer.body["schemas"][0],
        "urn:ietf:params:scim:api:messages:2.0:Error"
      );
    }
    // Discovery needs the token too: nobody learns that SCIM is set up.
    let answer = send_as(&app, None, Method::GET, "/ServiceProviderConfig", None, &[]).await;
    assert_eq!(answer.status, StatusCode::UNAUTHORIZED);
    // The SCIM token is no key to the API.
    let reply = app
      .http
      .get(format!("{}/api/v1/admin/users", app.base_url))
      .bearer_auth(&scim.token)
      .send()
      .await
      .unwrap();
    assert_eq!(reply.status(), StatusCode::UNAUTHORIZED);
    app.stop().await;
  }

  #[tokio::test]
  async fn a_rotated_token_replaces_the_old_one_and_a_revoked_client_is_gone() {
    let app = app!();
    let (scim, client_id) = client(&app, "external_id").await;
    let rotated = app
      .post(
        &admin(),
        &format!("/admin/scim/clients/{client_id}/token"),
        json!({}),
      )
      .await;
    assert_eq!(rotated.status, StatusCode::OK, "{}", rotated.body);
    assert_eq!(scim.get("/Users").await.status, StatusCode::UNAUTHORIZED);
    let fresh = Scim {
      app: &app,
      token: rotated.body["token"].as_str().unwrap().to_owned(),
    };
    assert_eq!(fresh.get("/Users").await.status, StatusCode::OK);

    let deleted = app
      .call(
        Method::DELETE,
        Some(&admin()),
        &format!("/admin/scim/clients/{client_id}"),
        None,
      )
      .await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT);
    assert_eq!(fresh.get("/Users").await.status, StatusCode::UNAUTHORIZED);
    app.stop().await;
  }

  #[tokio::test]
  async fn a_client_sees_only_its_own_resources() {
    let app = app!();
    let (first, _) = client(&app, "external_id").await;
    let (second, _) = client(&app, "external_id").await;
    let carol = first.create_user(OKTA_CREATE).await;
    assert_eq!(
      second.get(&format!("/Users/{carol}")).await.status,
      StatusCode::NOT_FOUND
    );
    assert_eq!(second.get("/Users").await.body["totalResults"], 0);
    // The same identity can't be taken over by the other client.
    let answer = second.post("/Users", OKTA_CREATE).await;
    assert_eq!(answer.status, StatusCode::CONFLICT, "{}", answer.body);
    assert_eq!(answer.body["error_code"], "scim_linked_elsewhere");
    app.stop().await;
  }

  #[tokio::test]
  async fn only_admins_manage_clients_and_tokens_are_shown_once() {
    let app = app!();
    let refused = app
      .post(
        &member("dave"),
        "/admin/scim/clients",
        json!({"name": "x", "identity_attribute": "external_id"}),
      )
      .await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    let (_scim, _) = client(&app, "user_name").await;
    let list = app.get(&admin(), "/admin/scim/clients").await;
    assert_eq!(list.status, StatusCode::OK);
    assert_eq!(list.body["clients"][0]["identity_attribute"], "user_name");
    assert!(!list.body.to_string().contains("tcscim_"), "no token in a list");
    for (name, attribute) in [("", "external_id"), ("x", "nonsense")] {
      let reply = app
        .post(
          &admin(),
          "/admin/scim/clients",
          json!({"name": name, "identity_attribute": attribute}),
        )
        .await;
      assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{name} {attribute}");
    }
    for _ in 1..teamcraft_proto::MAX_SCIM_CLIENTS {
      client(&app, "external_id").await;
    }
    let too_many = app
      .post(
        &admin(),
        "/admin/scim/clients",
        json!({"name": "x", "identity_attribute": "external_id"}),
      )
      .await;
    assert_eq!(too_many.status, StatusCode::CONFLICT);
    let unknown = app.post(&admin(), "/admin/scim/clients/1/token", json!({})).await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    assert_eq!(unknown.body["error_code"], "scim_client_not_found");
    app.stop().await;
  }

  #[tokio::test]
  async fn a_client_is_rate_limited() {
    let app = app!(|settings: &mut teamcraft_server::state::Settings| {
      settings.security.rate_limits.is_enabled = true;
      settings.security.rate_limits.ip_requests_per_minute = 10_000;
      settings.security.rate_limits.auth_failures_per_minute = 10_000;
      settings.security.rate_limits.user_writes_per_minute = 10_000;
      settings.scim.requests_per_minute = 3;
    });
    let (scim, _) = client(&app, "external_id").await;
    for _ in 0..3 {
      assert_eq!(scim.get("/Users").await.status, StatusCode::OK);
    }
    let limited = scim.get("/Users").await;
    assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(limited.body["error_code"], "rate_limited");
    assert!(limited.headers.contains_key("retry-after"));
    app.stop().await;
  }
}

mod users {
  use super::*;

  #[tokio::test]
  async fn a_created_user_is_the_account_the_first_sign_in_finds() {
    let app = app!();
    let (scim, _) = client(&app, "external_id").await;
    let created = scim.post("/Users", OKTA_CREATE).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    assert_eq!(
      created.headers.get("content-type").unwrap(),
      &HeaderValue::from_static("application/scim+json")
    );
    assert!(created.headers.contains_key("etag"));
    assert!(
      created
        .headers
        .get("location")
        .unwrap()
        .to_str()
        .unwrap()
        .ends_with(&format!(
          "/scim/v2/Users/{}",
          created.body["id"].as_str().unwrap()
        ))
    );
    let dave = created.body["id"].as_str().unwrap().to_owned();
    assert_eq!(created.body["userName"], "dave@example.com");
    assert_eq!(created.body["active"], true);
    assert_eq!(created.body["emails"][0]["value"], "dave@example.com");
    assert!(created.body.get("password").is_none());

    let listed = app.get(&admin(), "/users?query=dave").await;
    let entry = &listed.body["users"][0];
    assert_eq!(entry["id"], dave.as_str());
    assert_eq!(entry["is_awaiting_first_sign_in"], true);
    let audit = app
      .get(
        &owner(),
        &format!("/admin/audit?target_kind=user&target_id={dave}"),
      )
      .await;
    assert!(
      audit.body.to_string().contains("user_provisioned"),
      "{}",
      audit.body
    );
    assert!(audit.body.to_string().contains("scim"), "{}", audit.body);

    // The sign-in with subject `sub-dave` takes it; SCIM made a member, never more.
    assert_eq!(user_id(&app, &member("dave")).await, dave);
    let role: Option<String> =
      teamcraft_db::testing::sqlx::query_scalar("SELECT oidc_role FROM users WHERE id = $1::text::bigint")
        .bind(&dave)
        .fetch_one(app.sql_pool())
        .await
        .unwrap();
    assert_eq!(role.as_deref(), Some("member"));
    app.stop().await;
  }

  #[tokio::test]
  async fn the_shapes_of_entra_id_okta_and_keycloak_are_taken() {
    let app = app!();
    let (by_name, _) = client(&app, "user_name").await;
    let carol = by_name.create_user(ENTRA_CREATE).await;
    let got = by_name.get(&format!("/Users/{carol}")).await;
    assert_eq!(got.body["name"]["givenName"], "Carol");
    assert_eq!(got.body["displayName"], "Carol Example");
    // Entra's identity is its user name here: the sign-in of `sub-carol` finds the account.
    assert_eq!(user_id(&app, &member("carol")).await, carol);

    let (by_external_id, _) = client(&app, "external_id").await;
    by_external_id.create_user(OKTA_CREATE).await;
    let erin = by_external_id.create_user(KEYCLOAK_CREATE).await;
    assert_eq!(user_id(&app, &member("erin")).await, erin);
    app.stop().await;
  }

  #[tokio::test]
  async fn a_retried_create_answers_with_the_same_resource() {
    let app = app!();
    let (scim, _) = client(&app, "external_id").await;
    let first = scim.create_user(OKTA_CREATE).await;
    let second = scim.create_user(OKTA_CREATE).await;
    assert_eq!(first, second);
    assert_eq!(scim.get("/Users").await.body["totalResults"], 1);
    app.stop().await;
  }

  #[tokio::test]
  async fn an_existing_account_is_linked_not_made_again() {
    let app = app!();
    let dave = user_id(&app, &member("dave")).await;
    let (scim, _) = client(&app, "external_id").await;
    assert_eq!(scim.create_user(OKTA_CREATE).await, dave);
    // A signed-in account keeps the names of its sign-in.
    let entry = app.get(&admin(), "/users?query=dave").await;
    assert_eq!(entry.body["users"][0]["username"], "dave");
    app.stop().await;
  }

  #[tokio::test]
  async fn invalid_creates_are_refused_with_scim_errors() {
    let app = app!();
    let (scim, _) = client(&app, "external_id").await;
    let cases = [
      ("{not json", "scim_invalid_syntax", "invalidSyntax"),
      ("[]", "scim_invalid_syntax", "invalidSyntax"),
      (r#"{"externalId": "x"}"#, "scim_invalid_value", "invalidValue"),
      (r#"{"userName": "x"}"#, "scim_identity_immutable", "mutability"),
      (
        r#"{"userName": "x", "externalId": "y", "active": "perhaps"}"#,
        "scim_invalid_value",
        "invalidValue",
      ),
    ];
    for (body, code, scim_type) in cases {
      let answer = scim.post("/Users", body).await;
      assert_eq!(answer.status, StatusCode::BAD_REQUEST, "{body}: {}", answer.body);
      assert_eq!(answer.body["error_code"], code, "{body}");
      assert_eq!(answer.body["scimType"], scim_type, "{body}");
      assert_eq!(answer.body["status"], "400");
    }
    scim.create_user(OKTA_CREATE).await;
    let taken = scim
      .post(
        "/Users",
        r#"{"userName": "DAVE@example.com", "externalId": "sub-other"}"#,
      )
      .await;
    assert_eq!(taken.status, StatusCode::CONFLICT);
    assert_eq!(taken.body["scimType"], "uniqueness");
    app.stop().await;
  }

  #[tokio::test]
  async fn a_body_over_the_limit_is_refused() {
    let app = app!(|settings: &mut teamcraft_server::state::Settings| settings.scim.max_body_bytes = 512);
    let (scim, _) = client(&app, "external_id").await;
    let body = json!({"userName": "x", "externalId": "y", "title": "a".repeat(600)}).to_string();
    let answer = scim.post("/Users", &body).await;
    assert_eq!(answer.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(answer.body["error_code"], "scim_body_too_large");
    app.stop().await;
  }

  #[tokio::test]
  async fn filters_and_pages_find_users() {
    let app = app!();
    let (scim, _) = client(&app, "external_id").await;
    let mut ids = Vec::new();
    for index in 0..5 {
      ids.push(
        scim
          .create_user(
            &json!({"userName": format!("user{index}@example.com"), "externalId": format!("sub-u{index}")})
              .to_string(),
          )
          .await,
      );
    }
    let found = scim
      .get("/Users?filter=userName%20eq%20%22USER3%40example.com%22")
      .await;
    assert_eq!(found.body["totalResults"], 1, "{}", found.body);
    assert_eq!(found.body["Resources"][0]["id"], ids[3].as_str());
    let found = scim.get("/Users?filter=externalId%20eq%20%22sub-u1%22").await;
    assert_eq!(found.body["Resources"][0]["id"], ids[1].as_str());
    let found = scim.get("/Users?filter=externalId%20eq%20%22SUB-U1%22").await;
    assert_eq!(found.body["totalResults"], 0, "externalId compares exactly");
    let found = scim
      .get("/Users?filter=userName%20sw%20%22user%22%20and%20active%20eq%20true")
      .await;
    assert_eq!(found.body["totalResults"], 5);

    let page = scim.get("/Users?startIndex=2&count=2").await;
    assert_eq!(page.body["totalResults"], 5);
    assert_eq!(page.body["startIndex"], 2);
    assert_eq!(page.body["itemsPerPage"], 2);
    assert_eq!(page.body["Resources"][0]["id"], ids[1].as_str());
    let projected = scim.get("/Users?count=1&attributes=userName").await;
    assert!(projected.body["Resources"][0].get("emails").is_none());
    assert!(projected.body["Resources"][0].get("id").is_some());

    for bad in ["userName%20xx%20%22a%22", "(", "userName%20eq"] {
      let answer = scim.get(&format!("/Users?filter={bad}")).await;
      assert_eq!(answer.status, StatusCode::BAD_REQUEST, "{bad}");
      assert_eq!(answer.body["scimType"], "invalidFilter");
    }
    app.stop().await;
  }

  #[tokio::test]
  async fn a_filter_no_index_narrows_is_bounded() {
    let app = app!(|settings: &mut teamcraft_server::state::Settings| settings.scim.max_filter_scan = 2);
    let (scim, _) = client(&app, "external_id").await;
    for index in 0..3 {
      scim
        .create_user(&json!({"userName": format!("u{index}"), "externalId": format!("s{index}")}).to_string())
        .await;
    }
    let answer = scim.get("/Users?filter=active%20eq%20true").await;
    assert_eq!(answer.status, StatusCode::BAD_REQUEST);
    assert_eq!(answer.body["scimType"], "tooMany");
    // An equality on an indexed attribute still works.
    let answer = scim.get("/Users?filter=userName%20eq%20%22u1%22").await;
    assert_eq!(answer.body["totalResults"], 1);
    app.stop().await;
  }

  #[tokio::test]
  async fn put_and_patch_change_attributes_with_etags() {
    let app = app!();
    let (scim, _) = client(&app, "external_id").await;
    let dave = scim.create_user(OKTA_CREATE).await;
    let path = format!("/Users/{dave}");
    let got = scim.get(&path).await;
    let etag = got.headers.get("etag").unwrap().to_str().unwrap().to_owned();
    let not_modified = scim
      .send(Method::GET, &path, None, &[("if-none-match", &etag)])
      .await;
    assert_eq!(not_modified.status, StatusCode::NOT_MODIFIED);

    let replaced = scim
      .put(
        &path,
        r#"{"userName": "dave@example.com", "externalId": "sub-dave", "displayName": "David"}"#,
      )
      .await;
    assert_eq!(replaced.status, StatusCode::OK, "{}", replaced.body);
    assert_eq!(replaced.body["displayName"], "David");
    assert!(replaced.body.get("emails").is_none(), "a PUT replaces");
    // The old ETag is stale now.
    let stale = scim
      .send(
        Method::PATCH,
        &path,
        Some(&json!({"Operations": [{"op": "replace", "path": "displayName", "value": "D"}]}).to_string()),
        &[("if-match", &etag)],
      )
      .await;
    assert_eq!(stale.status, StatusCode::PRECONDITION_FAILED);
    assert_eq!(stale.body["error_code"], "scim_version_mismatch");

    let patched = scim
      .patch(
        &path,
        &json!({"schemas": ["urn:ietf:params:scim:api:messages:2.0:PatchOp"], "Operations": [
          {"op": "Replace", "path": "name.givenName", "value": "Dave"},
          {"op": "Add", "path": "emails[type eq \"work\"].value", "value": "dave@example.org"},
        ]}),
      )
      .await;
    assert_eq!(patched.status, StatusCode::OK, "{}", patched.body);
    assert_eq!(patched.body["name"]["givenName"], "Dave");
    assert_eq!(patched.body["emails"][0]["value"], "dave@example.org");
    // Before the first sign-in, the account shows what SCIM set.
    let entry = app.get(&admin(), "/users?query=dave").await;
    assert_eq!(entry.body["users"][0]["display_name"], "David");

    let immutable = scim
      .patch(
        &path,
        &json!({"Operations": [{"op": "replace", "path": "externalId", "value": "sub-other"}]}),
      )
      .await;
    assert_eq!(immutable.status, StatusCode::BAD_REQUEST);
    assert_eq!(immutable.body["error_code"], "scim_identity_immutable");
    let bad_path = scim
      .patch(
        &path,
        &json!({"Operations": [{"op": "replace", "path": "emails[", "value": "x"}]}),
      )
      .await;
    assert_eq!(bad_path.body["scimType"], "invalidPath");
    assert_eq!(scim.get("/Users/123").await.status, StatusCode::NOT_FOUND);
    assert_eq!(scim.get("/Users/not-an-id").await.status, StatusCode::NOT_FOUND);
    app.stop().await;
  }

  #[tokio::test]
  async fn disabling_deactivates_with_source_scim_and_enabling_brings_the_account_back() {
    let app = app!();
    let (scim, _) = client(&app, "external_id").await;
    let dave = scim.create_user(OKTA_CREATE).await;
    user_id(&app, &member("dave")).await;
    let path = format!("/Users/{dave}");

    // Entra ID sends `active` as text.
    let disabled = scim
      .patch(
        &path,
        &json!({"Operations": [{"op": "Replace", "path": "active", "value": "False"}]}),
      )
      .await;
    assert_eq!(disabled.status, StatusCode::OK, "{}", disabled.body);
    assert_eq!(disabled.body["active"], false);
    assert_eq!(
      app.get(&member("dave"), "/me").await.status,
      StatusCode::FORBIDDEN
    );
    let audit = app.get(&owner(), "/admin/audit?action=user_deactivated").await;
    let detail: Value = serde_json::from_str(audit.body["entries"][0]["detail"].as_str().unwrap()).unwrap();
    assert_eq!(
      (&detail["source"], &detail["reason"]),
      (&json!("scim"), &json!("oidc")),
      "{detail}"
    );
    // A leaver, as from the sync: owners' and admins' apps take them out of the groups.
    let reason: Option<String> = teamcraft_db::testing::sqlx::query_scalar(
      "SELECT deactivation_reason FROM users WHERE id = $1::text::bigint",
    )
    .bind(&dave)
    .fetch_one(app.sql_pool())
    .await
    .unwrap();
    assert_eq!(reason.as_deref(), Some("oidc"));
    // Disabling again changes nothing (an idempotent retry).
    let again = scim
      .patch(
        &path,
        &json!({"Operations": [{"op": "replace", "value": {"active": false}}]}),
      )
      .await;
    assert_eq!(again.status, StatusCode::OK);

    let enabled = scim
      .patch(
        &path,
        &json!({"Operations": [{"op": "replace", "value": {"active": true}}]}),
      )
      .await;
    assert_eq!(enabled.body["active"], true);
    assert_eq!(app.get(&member("dave"), "/me").await.status, StatusCode::OK);
    app.stop().await;
  }

  #[tokio::test]
  async fn an_admins_deactivation_is_not_lifted_by_scim() {
    let app = app!();
    let (scim, _) = client(&app, "external_id").await;
    let dave = scim.create_user(OKTA_CREATE).await;
    user_id(&app, &member("dave")).await;
    let by_admin = app
      .post(&admin(), &format!("/admin/users/{dave}/deactivate"), json!({}))
      .await;
    assert!(by_admin.status.is_success(), "{}", by_admin.body);
    scim
      .patch(
        &format!("/Users/{dave}"),
        &json!({"Operations": [{"op": "replace", "path": "active", "value": true}]}),
      )
      .await;
    assert_eq!(
      app.get(&member("dave"), "/me").await.status,
      StatusCode::FORBIDDEN
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn deleting_removes_a_never_signed_in_account_and_deactivates_others() {
    let app = app!();
    let (scim, _) = client(&app, "external_id").await;
    let erin = scim.create_user(KEYCLOAK_CREATE).await;
    let dave = scim.create_user(OKTA_CREATE).await;
    user_id(&app, &member("dave")).await;

    let deleted = scim.delete(&format!("/Users/{erin}")).await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT);
    assert_eq!(
      scim.get(&format!("/Users/{erin}")).await.status,
      StatusCode::NOT_FOUND
    );
    assert!(is_deleted(&app, &erin).await, "never signed in: deleted outright");
    // A retried delete finds nothing.
    assert_eq!(
      scim.delete(&format!("/Users/{erin}")).await.status,
      StatusCode::NOT_FOUND
    );

    assert_eq!(
      scim.delete(&format!("/Users/{dave}")).await.status,
      StatusCode::NO_CONTENT
    );
    assert!(
      !is_deleted(&app, &dave).await,
      "signed in: deactivated, not deleted"
    );
    assert_eq!(
      app.get(&member("dave"), "/me").await.status,
      StatusCode::FORBIDDEN
    );
    // The provider creates the person again: the same account comes back.
    assert_eq!(scim.create_user(OKTA_CREATE).await, dave);
    assert_eq!(app.get(&member("dave"), "/me").await.status, StatusCode::OK);
    app.stop().await;
  }

  #[tokio::test]
  async fn an_identity_an_admin_deleted_is_not_made_again() {
    let app = app!();
    let dave = user_id(&app, &member("dave")).await;
    let deleted = app
      .call(
        Method::DELETE,
        Some(&owner()),
        &format!("/admin/users/{dave}"),
        None,
      )
      .await;
    assert!(deleted.status.is_success(), "{}", deleted.body);
    let (scim, _) = client(&app, "external_id").await;
    let answer = scim.post("/Users", OKTA_CREATE).await;
    assert_eq!(answer.status, StatusCode::CONFLICT, "{}", answer.body);
    assert_eq!(answer.body["error_code"], "scim_identity_released");
    app.stop().await;
  }
}

mod brake {
  use super::*;

  #[tokio::test]
  async fn deactivations_beyond_the_window_wait_for_an_admin() {
    let app = app!(|settings: &mut teamcraft_server::state::Settings| {
      settings.scim.brake_max_percent = 0;
      settings.scim.brake_min_allowed = 1;
    });
    let (scim, _) = client(&app, "external_id").await;
    let dave = scim.create_user(OKTA_CREATE).await;
    let erin = scim.create_user(KEYCLOAK_CREATE).await;
    user_id(&app, &member("dave")).await;
    user_id(&app, &member("erin")).await;
    let disable = json!({"Operations": [{"op": "replace", "path": "active", "value": false}]});
    assert_eq!(
      scim.patch(&format!("/Users/{dave}"), &disable).await.body["active"],
      false
    );
    let held = scim.patch(&format!("/Users/{erin}"), &disable).await;
    assert_eq!(held.status, StatusCode::OK);
    assert_eq!(held.body["active"], true, "held: still active");
    assert_eq!(app.get(&member("erin"), "/me").await.status, StatusCode::OK);

    let status = app.get(&admin(), "/admin/oidc-sync").await;
    assert_eq!(
      status.body["held"][0]["user_id"],
      erin.as_str(),
      "{}",
      status.body
    );
    let revision = status.body["held_revision"].clone();
    let confirmed = app
      .post(
        &admin(),
        "/admin/oidc-sync/brake/confirm",
        json!({"revision": revision}),
      )
      .await;
    assert_eq!(confirmed.status, StatusCode::OK, "{}", confirmed.body);
    assert_eq!(
      app.get(&member("erin"), "/me").await.status,
      StatusCode::FORBIDDEN
    );
    assert!(
      confirmed.body["held"].as_array().unwrap().is_empty(),
      "{}",
      confirmed.body
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn an_admin_is_never_deactivated_by_scim_alone() {
    let app = app!();
    let (scim, _) = client(&app, "external_id").await;
    let bob = scim
      .create_user(r#"{"userName": "bob", "externalId": "sub-bob"}"#)
      .await;
    let held = scim
      .patch(
        &format!("/Users/{bob}"),
        &json!({"Operations": [{"op": "replace", "path": "active", "value": false}]}),
      )
      .await;
    assert_eq!(held.body["active"], true);
    assert_eq!(app.get(&admin(), "/me").await.status, StatusCode::OK);
    // A PATCH of other attributes (Entra ID sends them apart) keeps the hold.
    let renamed = scim
      .patch(
        &format!("/Users/{bob}"),
        &json!({"Operations": [{"op": "replace", "path": "displayName", "value": "Bob B"}]}),
      )
      .await;
    assert_eq!(renamed.status, StatusCode::OK, "{}", renamed.body);
    let status = app.get(&owner(), "/admin/oidc-sync").await;
    assert_eq!(status.body["held"][0]["user_id"], bob.as_str(), "{}", status.body);
    // So does a PUT that still says inactive.
    scim
      .put(
        &format!("/Users/{bob}"),
        r#"{"userName": "bob", "externalId": "sub-bob", "active": false}"#,
      )
      .await;
    let status = app.get(&owner(), "/admin/oidc-sync").await;
    assert_eq!(
      status.body["held"].as_array().unwrap().len(),
      1,
      "{}",
      status.body
    );
    // Enabling again takes the hold off the list.
    scim
      .patch(
        &format!("/Users/{bob}"),
        &json!({"Operations": [{"op": "replace", "path": "active", "value": true}]}),
      )
      .await;
    let status = app.get(&owner(), "/admin/oidc-sync").await;
    assert!(
      status.body["held"].as_array().unwrap().is_empty(),
      "{}",
      status.body
    );
    app.stop().await;
  }
}

mod brake_allowance {
  use super::*;

  #[tokio::test]
  async fn accounts_made_in_advance_do_not_raise_the_allowance() {
    let app = app!(|settings: &mut teamcraft_server::state::Settings| {
      settings.scim.brake_max_percent = 40;
      settings.scim.brake_min_allowed = 1;
    });
    let (scim, _) = client(&app, "external_id").await;
    let mut people = Vec::new();
    for name in ["carol", "dave", "erin"] {
      let id = scim
        .create_user(&json!({"userName": name, "externalId": format!("sub-{name}")}).to_string())
        .await;
      user_id(&app, &member(name)).await;
      people.push(id);
    }
    // Five people signed in: two deactivations per window. Twenty dummies would make it ten.
    for index in 0..20 {
      scim
        .create_user(
          &json!({"userName": format!("dummy{index}"), "externalId": format!("sub-dummy{index}")})
            .to_string(),
        )
        .await;
    }
    let disable = json!({"Operations": [{"op": "replace", "path": "active", "value": false}]});
    let mut states = Vec::new();
    for id in &people {
      states.push(scim.patch(&format!("/Users/{id}"), &disable).await.body["active"].clone());
    }
    assert_eq!(
      states,
      vec![json!(false), json!(false), json!(true)],
      "the third is held"
    );
    app.stop().await;
  }
}

mod groups {
  use super::*;

  #[tokio::test]
  async fn members_of_a_group_join_the_mapped_spaces_and_channels() {
    let app = app!();
    let space = create_space(&app, &admin(), "Engineering", "invite_only").await;
    let general = create_channel(&app, &admin(), &space, "general", "public").await;
    let reply = app
      .put(
        &admin(),
        "/admin/identity-provider/group-mappings",
        json!({"mappings": [{"group_path": "/engineering", "space_ids": [space], "channel_ids": [general]}]}),
      )
      .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let (scim, _) = client(&app, "external_id").await;
    let dave = scim.create_user(OKTA_CREATE).await;
    let erin = scim.create_user(KEYCLOAK_CREATE).await;

    let created = scim
      .post(
        "/Groups",
        &json!({"displayName": "Engineering", "externalId": "g-1", "members": [{"value": dave}]}).to_string(),
      )
      .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let group = created.body["id"].as_str().unwrap().to_owned();
    assert_eq!(created.body["members"][0]["value"], dave.as_str());
    assert!(
      is_member(&app, &general, &dave).await,
      "mapped on create, before a sign-in"
    );
    assert!(!is_member(&app, &general, &erin).await);

    // Entra ID adds by value.
    let added = scim
      .patch(
        &format!("/Groups/{group}"),
        &json!({"Operations": [{"op": "Add", "path": "members", "value": [{"value": erin}]}]}),
      )
      .await;
    assert_eq!(added.status, StatusCode::NO_CONTENT, "{}", added.body);
    assert!(added.headers.contains_key("etag"));
    assert!(is_member(&app, &general, &erin).await);
    // Okta removes by filter; leaving the group removes nobody from a channel.
    let removed = scim
      .patch(
        &format!("/Groups/{group}"),
        &json!({"Operations": [{"op": "remove", "path": format!("members[value eq \"{erin}\"]")}]}),
      )
      .await;
    assert_eq!(removed.status, StatusCode::NO_CONTENT);
    assert!(is_member(&app, &general, &erin).await);
    let user = scim.get(&format!("/Users/{dave}")).await;
    assert_eq!(user.body["groups"][0]["display"], "Engineering");

    let found = scim
      .get("/Groups?filter=displayName%20eq%20%22engineering%22&excludedAttributes=members")
      .await;
    assert_eq!(found.body["totalResults"], 1, "{}", found.body);
    assert!(found.body["Resources"][0].get("members").is_none());
    let with_member = scim
      .get(&format!("/Groups?filter=members%5Bvalue%20eq%20%22{dave}%22%5D"))
      .await;
    assert_eq!(with_member.body["totalResults"], 1, "{}", with_member.body);

    assert_eq!(
      scim.delete(&format!("/Groups/{group}")).await.status,
      StatusCode::NO_CONTENT
    );
    assert_eq!(
      scim.get(&format!("/Groups/{group}")).await.status,
      StatusCode::NOT_FOUND
    );
    assert!(is_member(&app, &general, &dave).await);
    app.stop().await;
  }

  #[tokio::test]
  async fn a_rule_saved_later_applies_to_existing_scim_groups() {
    let app = app!();
    let (scim, _) = client(&app, "external_id").await;
    let dave = scim.create_user(OKTA_CREATE).await;
    scim
      .post(
        "/Groups",
        &json!({"displayName": "Support", "members": [{"value": dave}]}).to_string(),
      )
      .await;
    let space = create_space(&app, &admin(), "Support", "invite_only").await;
    let channel = create_channel(&app, &admin(), &space, "tickets", "public").await;
    app
      .put(
        &admin(),
        "/admin/identity-provider/group-mappings",
        json!({"mappings": [{"group_path": "/support", "space_ids": [space], "channel_ids": [channel]}]}),
      )
      .await;
    teamcraft_server::services::scim::apply_mappings_to_all_groups(&app.state)
      .await
      .unwrap();
    assert!(is_member(&app, &channel, &dave).await);
    app.stop().await;
  }

  #[tokio::test]
  async fn invalid_group_writes_are_refused_and_retries_are_idempotent() {
    let app = app!(|settings: &mut teamcraft_server::state::Settings| settings.scim.max_group_members = 1);
    let (scim, _) = client(&app, "external_id").await;
    let (other, _) = client(&app, "external_id").await;
    let dave = scim.create_user(OKTA_CREATE).await;
    let erin = other.create_user(KEYCLOAK_CREATE).await;

    let foreign = scim
      .post(
        "/Groups",
        &json!({"displayName": "G", "members": [{"value": erin}]}).to_string(),
      )
      .await;
    assert_eq!(foreign.status, StatusCode::BAD_REQUEST);
    assert_eq!(foreign.body["error_code"], "scim_unknown_member");
    let nonsense = scim
      .post(
        "/Groups",
        &json!({"displayName": "G", "members": [{"value": "x"}]}).to_string(),
      )
      .await;
    assert_eq!(nonsense.body["error_code"], "scim_unknown_member");

    let body = json!({"displayName": "G", "externalId": "g", "members": [{"value": dave}]}).to_string();
    let first = scim.post("/Groups", &body).await;
    let second = scim.post("/Groups", &body).await;
    assert_eq!(first.body["id"], second.body["id"], "a retried create");
    let group = first.body["id"].as_str().unwrap().to_owned();
    let taken = scim
      .post("/Groups", r#"{"displayName": "g", "externalId": "other"}"#)
      .await;
    assert_eq!(taken.status, StatusCode::CONFLICT);

    // A second member is over the limit of this instance.
    let carol = scim
      .create_user(r#"{"userName": "carol", "externalId": "sub-carol"}"#)
      .await;
    let over = scim
      .patch(
        &format!("/Groups/{group}"),
        &json!({"Operations": [{"op": "add", "path": "members", "value": [{"value": carol}]}]}),
      )
      .await;
    assert_eq!(over.status, StatusCode::BAD_REQUEST);
    assert_eq!(over.body["scimType"], "tooMany");
    // Adding a member twice changes nothing.
    let again = scim
      .patch(
        &format!("/Groups/{group}"),
        &json!({"Operations": [{"op": "add", "path": "members", "value": [{"value": dave}]}]}),
      )
      .await;
    assert_eq!(again.status, StatusCode::NO_CONTENT, "{}", again.body);
    let group = scim.get(&format!("/Groups/{group}")).await;
    assert_eq!(group.body["members"].as_array().unwrap().len(), 1);
    app.stop().await;
  }
}

mod group_filters {
  use super::*;

  #[tokio::test]
  async fn a_filter_on_members_reads_a_bounded_number_of_them() {
    let app = app!(|settings: &mut teamcraft_server::state::Settings| settings.scim.max_filter_members = 2);
    let (scim, _) = client(&app, "external_id").await;
    let mut members = Vec::new();
    for name in ["carol", "dave", "erin"] {
      let id = scim
        .create_user(&json!({"userName": name, "externalId": format!("sub-{name}")}).to_string())
        .await;
      members.push(json!({"value": id}));
    }
    let group = scim
      .post(
        "/Groups",
        &json!({"displayName": "G", "members": members}).to_string(),
      )
      .await;
    assert_eq!(group.status, StatusCode::CREATED, "{}", group.body);
    let dave = members[1]["value"].as_str().unwrap();
    let answer = scim
      .get(&format!("/Groups?filter=members%5Bvalue%20eq%20%22{dave}%22%5D"))
      .await;
    assert_eq!(answer.status, StatusCode::BAD_REQUEST, "{}", answer.body);
    assert_eq!(answer.body["scimType"], "tooMany");
    // Without members in the filter nothing is read.
    let answer = scim.get("/Groups?filter=displayName%20eq%20%22G%22").await;
    assert_eq!(answer.body["totalResults"], 1);
    app.stop().await;
  }
}

mod discovery {
  use super::*;

  #[tokio::test]
  async fn the_provider_reads_what_is_supported() {
    let app = app!();
    let (scim, _) = client(&app, "external_id").await;
    let config = scim.get("/ServiceProviderConfig").await;
    assert_eq!(config.body["patch"]["supported"], true);
    assert_eq!(config.body["bulk"]["supported"], false);
    assert_eq!(config.body["etag"]["supported"], true);
    let types = scim.get("/ResourceTypes").await;
    assert_eq!(types.body["totalResults"], 2);
    assert_eq!(scim.get("/ResourceTypes/User").await.body["endpoint"], "/Users");
    let schemas = scim.get("/Schemas").await;
    assert_eq!(schemas.body["totalResults"], 2);
    assert_eq!(
      scim
        .get("/Schemas/urn:ietf:params:scim:schemas:core:2.0:Group")
        .await
        .body["name"],
      "Group"
    );
    let unknown = scim.get("/Nonsense").await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    assert_eq!(unknown.body["error_code"], "scim_not_found");
    app.stop().await;
  }
}

async fn is_member(app: &TestApp, channel: &str, user: &str) -> bool {
  teamcraft_db::testing::sqlx::query_scalar::<_, bool>(
    "SELECT EXISTS (SELECT 1 FROM memberships WHERE conversation_id = $1::text::bigint AND user_id = $2::text::bigint)",
  )
  .bind(channel)
  .bind(user)
  .fetch_one(app.sql_pool())
  .await
  .unwrap()
}

async fn is_deleted(app: &TestApp, user: &str) -> bool {
  teamcraft_db::testing::sqlx::query_scalar::<_, bool>(
    "SELECT deleted_at IS NOT NULL FROM users WHERE id = $1::text::bigint",
  )
  .bind(user)
  .fetch_one(app.sql_pool())
  .await
  .unwrap()
}
