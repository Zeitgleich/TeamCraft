use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};

/// A share from a browser whose service worker didn't take it (docs/e2ee.md §9.3): the body is
/// plaintext the server must not see. It is never read: the answer sends the browser back to the app,
/// which says the share failed, and the connection closes so the rest of the body isn't taken in.
#[utoipa::path(post, path = "/share-target", tag = "brand", summary = "Refuses a share that didn't stay in the browser, unread.", responses((status = 303, description = "Back to the app, which says the share failed")))]
pub async fn post_share_target_handler() -> Response {
  (
    StatusCode::SEE_OTHER,
    [
      (header::LOCATION, HeaderValue::from_static("/?share-failed")),
      (header::CONNECTION, HeaderValue::from_static("close")),
    ],
  )
    .into_response()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[tokio::test]
  async fn a_share_is_sent_back_and_the_connection_closes() {
    let response = post_share_target_handler().await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(response.headers()[header::LOCATION], "/?share-failed");
    assert_eq!(response.headers()[header::CONNECTION], "close");
  }
}
