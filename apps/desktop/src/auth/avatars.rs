//! Profile pictures through the shell. They live in object storage behind presigned URLs, and the
//! webview's content security policy keeps object storage out of `connect-src`, so the shell asks
//! the API for the presigned URL and moves the bytes itself. The URL is always the one in the API
//! answer the shell has just read, never one the page passes in.

use std::time::Duration;

use base64::Engine;
use teamcraft_client_api::remote::ApiMethod;
use teamcraft_client_api::server_entry::HttpPolicy;
use teamcraft_proto::{
  AvatarContentType, AvatarResponse, CreateAvatarUploadRequest, CreateAvatarUploadResponse, UserId,
};

use super::store::TokenStore;
use super::{ApiBytesResponse, ApiResponse, AuthError, AuthService, HTTP_POLICY};

/// The largest picture moved either way. The server's own limit (`max_avatar_bytes`) is lower by
/// default; this only keeps a hostile answer from making the shell buffer an unbounded body.
pub const MAX_AVATAR_BYTES: u64 = 16 * 1024 * 1024;
/// A whole transfer to or from object storage; longer than an API call, for slow links.
const OBJECT_TIMEOUT: Duration = Duration::from_secs(60);
/// Answer of a refused storage upload, in the API's error shape so the page maps it like one.
const UPLOAD_REFUSED_BODY: &str =
  r#"{"error_code":"avatar_upload_failed","message":"object storage refused the upload"}"#;

impl<S: TokenStore> AuthService<S> {
  /// The picture of `user`: the API names the presigned URL, the shell downloads it. An API refusal
  /// or a storage error comes back as its status with an empty body.
  pub async fn avatar_download(&self, user: UserId) -> Result<ApiBytesResponse, AuthError> {
    let answer = self
      .api_request(ApiMethod::Get, &format!("/api/v1/users/{user}/avatar"), None)
      .await?;
    if answer.status >= 300 {
      return Ok(empty_bytes(answer.status));
    }
    let location: AvatarResponse = parse(&answer.body)?;
    let url = presigned_url(&location.download_url, HTTP_POLICY)?;
    let max_bytes = location.size_bytes.min(MAX_AVATAR_BYTES);
    let (status, body) = get_object(&self.http, url, max_bytes).await?;
    if !(200..300).contains(&status) {
      return Ok(empty_bytes(status));
    }
    Ok(ApiBytesResponse {
      status,
      content_type: Some(location.content_type.as_str().to_owned()),
      body_base64: base64::engine::general_purpose::STANDARD.encode(&body),
    })
  }

  /// Makes `bytes` the signed-in user's picture: announces it, uploads it to the presigned URL the
  /// API answers with, and completes it. Returns the answer of the first API step that refused, a
  /// refusal in the API's shape when storage refused, or the completion's answer (the profile).
  pub async fn avatar_upload(
    &self,
    content_type: AvatarContentType,
    bytes: Vec<u8>,
  ) -> Result<ApiResponse, AuthError> {
    check_upload(&content_type, bytes.len())?;
    let announce = CreateAvatarUploadRequest {
      content_type: content_type.clone(),
      size_bytes: bytes.len() as u64,
    };
    let body =
      serde_json::to_string(&announce).map_err(|error| AuthError::InvalidResponse(error.to_string()))?;
    let answer = self
      .api_request(ApiMethod::Post, "/api/v1/me/avatar", Some(body))
      .await?;
    if answer.status >= 300 {
      return Ok(answer);
    }
    let upload: CreateAvatarUploadResponse = parse(&answer.body)?;
    let url = presigned_url(&upload.upload_url, HTTP_POLICY)?;
    let status = put_object(&self.http, url, content_type.as_str(), bytes).await?;
    if !(200..300).contains(&status) {
      return Ok(ApiResponse {
        status,
        body: UPLOAD_REFUSED_BODY.to_owned(),
      });
    }
    self
      .api_request(
        ApiMethod::Post,
        "/api/v1/me/avatar/complete",
        Some("{}".to_owned()),
      )
      .await
  }
}

/// Only the image types the server takes, and nothing above [`MAX_AVATAR_BYTES`].
fn check_upload(content_type: &AvatarContentType, size_bytes: usize) -> Result<(), AuthError> {
  if let AvatarContentType::Unknown(other) = content_type {
    return Err(AuthError::Unsupported(format!("{other:?} is not a picture type")));
  }
  if size_bytes as u64 > MAX_AVATAR_BYTES {
    return Err(AuthError::Unsupported("the picture is too large".to_owned()));
  }
  Ok(())
}

/// A presigned URL from an API answer, if the shell may send a request there: `https`, or plain
/// `http` as far as `policy` allows (a storage server on this computer in release builds), and no
/// credentials in the URL itself (presigned URLs carry their signature in the query).
fn presigned_url(raw: &str, policy: HttpPolicy) -> Result<reqwest::Url, AuthError> {
  let refuse = |reason: &str| AuthError::InvalidResponse(format!("the storage URL {reason}"));
  let url = reqwest::Url::parse(raw).map_err(|_| refuse("is not a URL"))?;
  if !url.username().is_empty() || url.password().is_some() {
    return Err(refuse("carries credentials"));
  }
  let is_loopback = match url.host() {
    Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
    Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
    Some(url::Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
    None => return Err(refuse("has no host")),
  };
  match (url.scheme(), policy) {
    ("https", _) | ("http", HttpPolicy::Allowed) => Ok(url),
    ("http", HttpPolicy::LoopbackOnly) if is_loopback => Ok(url),
    ("http", HttpPolicy::LoopbackOnly) => Err(refuse("is plain http to another computer")),
    _ => Err(refuse("is not http(s)")),
  }
}

/// `GET url` without credentials (the URL is the credential); `(status, body)`. Fails once the body
/// grows past `max_bytes`, announced or not. Redirects are not followed (the client's policy).
async fn get_object(
  http: &reqwest::Client,
  url: reqwest::Url,
  max_bytes: u64,
) -> Result<(u16, Vec<u8>), AuthError> {
  let too_large = || AuthError::InvalidResponse("the picture is larger than announced".to_owned());
  let mut response = http
    .get(url)
    .timeout(OBJECT_TIMEOUT)
    .send()
    .await
    .map_err(network)?;
  let status = response.status().as_u16();
  if !response.status().is_success() {
    return Ok((status, Vec::new()));
  }
  if response.content_length().is_some_and(|length| length > max_bytes) {
    return Err(too_large());
  }
  let mut body = Vec::new();
  while let Some(chunk) = response.chunk().await.map_err(network)? {
    if (body.len() + chunk.len()) as u64 > max_bytes {
      return Err(too_large());
    }
    body.extend_from_slice(&chunk);
  }
  Ok((status, body))
}

/// `PUT url` with the picture, without credentials; the storage's status.
async fn put_object(
  http: &reqwest::Client,
  url: reqwest::Url,
  content_type: &str,
  bytes: Vec<u8>,
) -> Result<u16, AuthError> {
  let response = http
    .put(url)
    .timeout(OBJECT_TIMEOUT)
    .header(reqwest::header::CONTENT_TYPE, content_type)
    .body(bytes)
    .send()
    .await
    .map_err(network)?;
  Ok(response.status().as_u16())
}

fn parse<T: serde::de::DeserializeOwned>(body: &str) -> Result<T, AuthError> {
  serde_json::from_str(body).map_err(|error| AuthError::InvalidResponse(error.to_string()))
}

fn empty_bytes(status: u16) -> ApiBytesResponse {
  ApiBytesResponse {
    status,
    content_type: None,
    body_base64: String::new(),
  }
}

fn network(error: reqwest::Error) -> AuthError {
  AuthError::Network(error.to_string())
}

#[cfg(test)]
mod tests {
  use tokio::io::{AsyncReadExt, AsyncWriteExt};
  use tokio::net::TcpListener;

  use super::*;
  use crate::auth::store::MemoryStore;

  mod presigned_urls {
    use super::*;

    #[test]
    fn https_is_taken() {
      let url = presigned_url(
        "https://s3.example.com/bucket/a?X-Amz-Signature=1",
        HttpPolicy::LoopbackOnly,
      );
      assert_eq!(url.unwrap().host_str(), Some("s3.example.com"));
    }

    #[test]
    fn plain_http_only_to_this_computer_in_release_builds() {
      for local in [
        "http://127.0.0.1:9000/a",
        "http://[::1]:9000/a",
        "http://localhost:9000/a",
      ] {
        assert!(presigned_url(local, HttpPolicy::LoopbackOnly).is_ok(), "{local}");
      }
      assert!(presigned_url("http://s3.example.com/a", HttpPolicy::LoopbackOnly).is_err());
      assert!(presigned_url("http://s3.example.com/a", HttpPolicy::Allowed).is_ok());
    }

    #[test]
    fn other_schemes_and_odd_urls_are_refused() {
      for raw in [
        "file:///etc/passwd",
        "ftp://s3.example.com/a",
        "javascript:alert(1)",
        "data:image/png;base64,AAAA",
        "/relative/path",
        "",
        "https://user:secret@s3.example.com/a",
      ] {
        assert!(presigned_url(raw, HttpPolicy::Allowed).is_err(), "{raw}");
      }
    }
  }

  mod limits {
    use super::*;

    #[tokio::test]
    async fn a_picture_within_the_limit_is_read() {
      let url = serve_once("200 OK", Some(4), b"abcd".to_vec()).await;
      let (status, body) = get_object(&client(), url, 4).await.unwrap();
      assert_eq!((status, body), (200, b"abcd".to_vec()));
    }

    #[tokio::test]
    async fn an_announced_length_above_the_limit_is_refused() {
      let url = serve_once("200 OK", Some(5), b"abcde".to_vec()).await;
      assert!(matches!(
        get_object(&client(), url, 4).await,
        Err(AuthError::InvalidResponse(_))
      ));
    }

    #[tokio::test]
    async fn an_unannounced_body_stops_at_the_limit() {
      let url = serve_once("200 OK", None, vec![b'x'; 64 * 1024]).await;
      assert!(matches!(
        get_object(&client(), url, 1024).await,
        Err(AuthError::InvalidResponse(_))
      ));
    }

    #[tokio::test]
    async fn a_redirect_is_not_followed() {
      let url = serve_once("302 Found", Some(0), Vec::new()).await;
      let (status, body) = get_object(&client(), url, 4).await.unwrap();
      assert_eq!((status, body.len()), (302, 0));
    }

    #[tokio::test]
    async fn an_upload_above_the_limit_never_leaves_the_shell() {
      let service = AuthService::new(None, MemoryStore::default()).unwrap();
      let too_large = vec![0; MAX_AVATAR_BYTES as usize + 1];
      assert!(matches!(
        service.avatar_upload(AvatarContentType::Png, too_large).await,
        Err(AuthError::Unsupported(_))
      ));
    }

    #[tokio::test]
    async fn an_upload_of_an_unknown_type_is_refused() {
      let service = AuthService::new(None, MemoryStore::default()).unwrap();
      let svg = AvatarContentType::Unknown("image/svg+xml".to_owned());
      assert!(matches!(
        service.avatar_upload(svg, vec![1]).await,
        Err(AuthError::Unsupported(_))
      ));
    }
  }

  /// The shell's own client setup: no redirects followed.
  fn client() -> reqwest::Client {
    reqwest::Client::builder()
      .redirect(reqwest::redirect::Policy::none())
      .build()
      .unwrap()
  }

  /// Answers one request on a loopback port with `status` and `body`; without `maybe_length` the
  /// body is not announced and ends with the connection.
  async fn serve_once(status: &'static str, maybe_length: Option<usize>, body: Vec<u8>) -> reqwest::Url {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
      let (mut stream, _) = listener.accept().await.unwrap();
      let mut request = [0; 4096];
      let _ = stream.read(&mut request).await;
      let mut head =
        format!("HTTP/1.1 {status}\r\nConnection: close\r\nLocation: https://elsewhere.example.com/\r\n");
      if let Some(length) = maybe_length {
        head.push_str(&format!("Content-Length: {length}\r\n"));
      }
      head.push_str("\r\n");
      // The client may hang up early once it has seen enough; that is what some tests check.
      let _ = stream.write_all(head.as_bytes()).await;
      let _ = stream.write_all(&body).await;
    });
    reqwest::Url::parse(&format!("http://{address}/object")).unwrap()
  }
}
