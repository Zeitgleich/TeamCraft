//! The CalDAV side: meetings are stored as events in a calendar collection of the calendar server
//! (CalCraft in the reference setup).

use std::time::Duration;

use reqwest::StatusCode;
use secrecy::ExposeSecret;

use crate::auth::BoxFuture;
use crate::config::CalDavConfig;

/// Writes meeting events to a calendar.
pub trait CalendarStore: Send + Sync {
  /// Creates or replaces the event stored as `object_name` (without extension).
  fn put_event<'a>(&'a self, object_name: &'a str, ics: &'a str) -> BoxFuture<'a, Result<(), CalendarError>>;

  /// Removes the event; an event that is already gone is not an error.
  fn delete_event<'a>(&'a self, object_name: &'a str) -> BoxFuture<'a, Result<(), CalendarError>>;
}

#[derive(Debug, thiserror::Error)]
pub enum CalendarError {
  #[error("the calendar server could not be reached: {0}")]
  Unreachable(String),
  #[error("the calendar server answered with HTTP status {0}")]
  Rejected(u16),
}

/// CalDAV over HTTP with basic authentication (an app password in the reference setup).
pub struct CalDavStore {
  client: reqwest::Client,
  calendar_url: String,
  username: String,
  password: secrecy::SecretString,
}

impl CalDavStore {
  pub fn new(config: &CalDavConfig) -> Result<Self, reqwest::Error> {
    let client = reqwest::Client::builder()
      .timeout(Duration::from_secs(config.request_timeout_secs))
      .build()?;
    Ok(Self {
      client,
      calendar_url: config.calendar_url.trim_end_matches('/').to_owned(),
      username: config.username.clone(),
      password: config.password.clone(),
    })
  }

  fn object_url(&self, object_name: &str) -> String {
    format!("{}/{object_name}.ics", self.calendar_url)
  }
}

impl CalendarStore for CalDavStore {
  fn put_event<'a>(&'a self, object_name: &'a str, ics: &'a str) -> BoxFuture<'a, Result<(), CalendarError>> {
    Box::pin(async move {
      let response = self
        .client
        .put(self.object_url(object_name))
        .basic_auth(&self.username, Some(self.password.expose_secret()))
        .header(reqwest::header::CONTENT_TYPE, "text/calendar; charset=utf-8")
        .body(ics.to_owned())
        .send()
        .await
        .map_err(|error| CalendarError::Unreachable(error.to_string()))?;
      match response.status() {
        StatusCode::OK | StatusCode::CREATED | StatusCode::NO_CONTENT => Ok(()),
        other => Err(CalendarError::Rejected(other.as_u16())),
      }
    })
  }

  fn delete_event<'a>(&'a self, object_name: &'a str) -> BoxFuture<'a, Result<(), CalendarError>> {
    Box::pin(async move {
      let response = self
        .client
        .delete(self.object_url(object_name))
        .basic_auth(&self.username, Some(self.password.expose_secret()))
        .send()
        .await
        .map_err(|error| CalendarError::Unreachable(error.to_string()))?;
      match response.status() {
        StatusCode::OK | StatusCode::NO_CONTENT | StatusCode::NOT_FOUND => Ok(()),
        other => Err(CalendarError::Rejected(other.as_u16())),
      }
    })
  }
}

#[cfg(test)]
mod tests {
  use std::collections::BTreeMap;
  use std::sync::{Arc, Mutex};

  use axum::Router;
  use axum::body::Bytes;
  use axum::extract::{Path, State};
  use axum::http::{HeaderMap, StatusCode as HttpStatus};
  use axum::routing::put;

  use super::*;

  type Objects = Arc<Mutex<BTreeMap<String, (String, String)>>>;

  #[tokio::test]
  async fn events_are_put_and_deleted_with_basic_auth_and_calendar_content_type() {
    let objects = Objects::default();
    let store = store_for(serve(objects.clone(), false).await);

    store
      .put_event("42", "BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n")
      .await
      .unwrap();

    let stored = objects.lock().unwrap().get("42.ics").cloned().unwrap();
    assert_eq!(stored.0, "text/calendar; charset=utf-8");
    // "teamcraft:app-password" as basic credentials.
    assert_eq!(stored.1, "Basic dGVhbWNyYWZ0OmFwcC1wYXNzd29yZA==");
    store.delete_event("42").await.unwrap();
    assert!(objects.lock().unwrap().is_empty());
    store.delete_event("42").await.unwrap();
  }

  #[tokio::test]
  async fn rejections_and_unreachable_servers_are_reported() {
    let rejecting = store_for(serve(Objects::default(), true).await);
    assert!(matches!(
      rejecting.put_event("1", "x").await,
      Err(CalendarError::Rejected(403))
    ));
    assert!(matches!(
      rejecting.delete_event("1").await,
      Err(CalendarError::Rejected(403))
    ));

    let closed = {
      let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
      listener.local_addr().unwrap()
    };
    assert!(matches!(
      store_for(closed).put_event("1", "x").await,
      Err(CalendarError::Unreachable(_))
    ));
  }

  fn store_for(addr: std::net::SocketAddr) -> CalDavStore {
    CalDavStore::new(&CalDavConfig {
      calendar_url: format!("http://{addr}/dav/calendars/teamcraft/meetings/"),
      username: "teamcraft".into(),
      password: "app-password".to_owned().into(),
      request_timeout_secs: 2,
    })
    .unwrap()
  }

  async fn serve(objects: Objects, is_forbidding: bool) -> std::net::SocketAddr {
    async fn put_object(
      State((objects, is_forbidding)): State<(Objects, bool)>,
      Path(name): Path<String>,
      headers: HeaderMap,
      _body: Bytes,
    ) -> HttpStatus {
      if is_forbidding {
        return HttpStatus::FORBIDDEN;
      }
      let header = |key: &str| {
        headers
          .get(key)
          .and_then(|v| v.to_str().ok())
          .unwrap_or("")
          .to_owned()
      };
      objects
        .lock()
        .unwrap()
        .insert(name, (header("content-type"), header("authorization")));
      HttpStatus::CREATED
    }
    async fn delete_object(
      State((objects, is_forbidding)): State<(Objects, bool)>,
      Path(name): Path<String>,
    ) -> HttpStatus {
      if is_forbidding {
        return HttpStatus::FORBIDDEN;
      }
      match objects.lock().unwrap().remove(&name) {
        Some(_) => HttpStatus::NO_CONTENT,
        None => HttpStatus::NOT_FOUND,
      }
    }
    let app = Router::new()
      .route(
        "/dav/calendars/teamcraft/meetings/{name}",
        put(put_object).delete(delete_object),
      )
      .with_state((objects, is_forbidding));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await });
    addr
  }
}
