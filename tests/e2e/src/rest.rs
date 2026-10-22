//! Plain REST calls for what the client core doesn't do (space setup, admin, meetings, retention).

use reqwest::{Method, StatusCode};
use serde_json::Value;

use crate::stack::Stack;
use crate::tokens::UserTokens;

/// Status and JSON body of one answer (`Null` when the body isn't JSON).
#[derive(Debug)]
pub struct Reply {
  pub status: StatusCode,
  pub body: Value,
}

impl Reply {
  pub fn id(&self) -> String {
    self.body["id"].as_str().unwrap_or_default().to_owned()
  }

  pub fn error_code(&self) -> &str {
    self.body["error_code"].as_str().unwrap_or_default()
  }

  /// Fails the test with the body unless the status is `expected`.
  pub fn expect(self, expected: StatusCode) -> Self {
    assert_eq!(self.status, expected, "unexpected answer: {}", self.body);
    self
  }
}

impl Stack {
  pub async fn get(&self, user: &UserTokens, path: &str) -> Reply {
    self.request(user, Method::GET, path, None).await
  }

  pub async fn post(&self, user: &UserTokens, path: &str, body: Value) -> Reply {
    self.request(user, Method::POST, path, Some(body)).await
  }

  pub async fn put(&self, user: &UserTokens, path: &str, body: Value) -> Reply {
    self.request(user, Method::PUT, path, Some(body)).await
  }

  pub async fn patch(&self, user: &UserTokens, path: &str, body: Value) -> Reply {
    self.request(user, Method::PATCH, path, Some(body)).await
  }

  pub async fn delete(&self, user: &UserTokens, path: &str) -> Reply {
    self.request(user, Method::DELETE, path, None).await
  }

  /// A request with an arbitrary bearer token (guest sessions, bots) or none.
  pub async fn request_with(
    &self,
    maybe_token: Option<&str>,
    method: Method,
    path: &str,
    maybe_body: Option<Value>,
  ) -> Reply {
    let mut request = self
      .http
      .request(method, format!("{}/api/v1{path}", self.base_url));
    if let Some(token) = maybe_token {
      request = request.bearer_auth(token);
    }
    if let Some(body) = maybe_body {
      request = request.json(&body);
    }
    let response = request.send().await.expect("request reaches the server");
    let status = response.status();
    let body = response.json().await.unwrap_or(Value::Null);
    Reply { status, body }
  }

  async fn request(&self, user: &UserTokens, method: Method, path: &str, maybe_body: Option<Value>) -> Reply {
    let token = user.token_async().await;
    self.request_with(Some(&token), method, path, maybe_body).await
  }

  /// A space (`join_policy`: `open` or `invite_only`); returns its ID.
  pub async fn create_space(&self, owner: &UserTokens, name: &str, join_policy: &str) -> String {
    self
      .post(
        owner,
        "/spaces",
        serde_json::json!({"name": name, "join_policy": join_policy}),
      )
      .await
      .expect(StatusCode::CREATED)
      .id()
  }

  /// Adds a user to a space with a role (`admin`, `member`, `guest`).
  pub async fn add_space_member(&self, admin: &UserTokens, space: &str, user_id: &str, role: &str) {
    self
      .post(
        admin,
        &format!("/spaces/{space}/members"),
        serde_json::json!({"user_id": user_id, "role": role}),
      )
      .await
      .expect(StatusCode::CREATED);
  }

  /// The user's ID as the server knows it.
  pub async fn user_id(&self, user: &UserTokens) -> String {
    self.get(user, "/me").await.expect(StatusCode::OK).id()
  }
}
