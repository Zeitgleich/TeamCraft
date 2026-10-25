//! Profile pictures through the shell. They live in object storage behind presigned URLs, and the
//! webview's content security policy keeps object storage out of `connect-src`, so the shell asks
//! the API for the presigned URL and moves the bytes itself. The URL is always the one in the API
//! answer the shell has just read, never one the page passes in.

use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, OnceLock};
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

/// Where the shell may send a presigned URL. The URL comes from the server, so a hostile server
/// must not be able to point the shell at services on this computer or its network, which have no
/// CORS or private-network protection against the shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StorageReach {
  /// Development builds: anywhere, plain `http` included.
  Anywhere,
  /// The server runs on this computer (a local test setup): storage may too, over plain `http`.
  ThisComputer,
  /// Everyone else: `https` to public addresses only, also after name resolution.
  PublicOnly,
}

impl<S: TokenStore> AuthService<S> {
  /// The picture of `user`: the API names the presigned URL, the shell downloads it. An API refusal
  /// or a storage error comes back as its status with an empty body.
  pub async fn avatar_download(&self, user: UserId) -> Result<ApiBytesResponse, AuthError> {
    let reach = self.storage_reach()?;
    let answer = self
      .api_request(ApiMethod::Get, &format!("/api/v1/users/{user}/avatar"), None)
      .await?;
    if answer.status >= 300 {
      return Ok(empty_bytes(answer.status));
    }
    let location: AvatarResponse = parse(&answer.body)?;
    if let AvatarContentType::Unknown(other) = &location.content_type {
      return Err(AuthError::InvalidResponse(format!(
        "{other:?} is not a picture type"
      )));
    }
    let url = presigned_url(&location.download_url, reach)?;
    let max_bytes = location.size_bytes.min(MAX_AVATAR_BYTES);
    let content_type = location.content_type.as_str();
    let (status, body) = get_object(self.object_client(reach)?, url, max_bytes, content_type).await?;
    if !(200..300).contains(&status) {
      return Ok(empty_bytes(status));
    }
    Ok(ApiBytesResponse {
      status,
      content_type: Some(content_type.to_owned()),
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
    let reach = self.storage_reach()?;
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
    let url = presigned_url(&upload.upload_url, reach)?;
    let status = put_object(self.object_client(reach)?, url, content_type.as_str(), bytes).await?;
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

  /// How far presigned URLs of the active server may reach.
  fn storage_reach(&self) -> Result<StorageReach, AuthError> {
    if HTTP_POLICY == HttpPolicy::Allowed {
      return Ok(StorageReach::Anywhere);
    }
    let server = self.server()?;
    let is_local = reqwest::Url::parse(&server.server_url)
      .ok()
      .is_some_and(|url| host_class(&url) == Some(HostClass::ThisComputer));
    Ok(if is_local {
      StorageReach::ThisComputer
    } else {
      StorageReach::PublicOnly
    })
  }

  /// The shell's client, or for [`StorageReach::PublicOnly`] one that only connects to public
  /// addresses (a public name may still resolve to a private one).
  fn object_client(&self, reach: StorageReach) -> Result<&reqwest::Client, AuthError> {
    match reach {
      StorageReach::Anywhere | StorageReach::ThisComputer => Ok(&self.http),
      StorageReach::PublicOnly => public_client(),
    }
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

/// A presigned URL from an API answer, if the shell may send a request there under `reach`, with
/// no credentials in the URL itself (presigned URLs carry their signature in the query).
fn presigned_url(raw: &str, reach: StorageReach) -> Result<reqwest::Url, AuthError> {
  let refuse = |reason: &str| AuthError::InvalidResponse(format!("the storage URL {reason}"));
  let url = reqwest::Url::parse(raw).map_err(|_| refuse("is not a URL"))?;
  if !url.username().is_empty() || url.password().is_some() {
    return Err(refuse("carries credentials"));
  }
  let is_https = match url.scheme() {
    "https" => true,
    "http" => false,
    _ => return Err(refuse("is not http(s)")),
  };
  let class = host_class(&url).ok_or_else(|| refuse("has no host"))?;
  match (reach, class) {
    (StorageReach::Anywhere, _) => Ok(url),
    (StorageReach::ThisComputer, HostClass::ThisComputer) => Ok(url),
    (StorageReach::ThisComputer, _) | (StorageReach::PublicOnly, HostClass::Public) if is_https => Ok(url),
    (StorageReach::PublicOnly, HostClass::ThisComputer | HostClass::LocalNetwork) => {
      Err(refuse("points into this computer or its network"))
    },
    _ => Err(refuse("is plain http to another computer")),
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostClass {
  /// `localhost` or a loopback address.
  ThisComputer,
  /// A private, link-local, unique-local, shared or unspecified address literal.
  LocalNetwork,
  /// A public address literal or any other name (checked again when it resolves).
  Public,
}

fn host_class(url: &reqwest::Url) -> Option<HostClass> {
  match url.host()? {
    url::Host::Domain(name) if name.eq_ignore_ascii_case("localhost") => Some(HostClass::ThisComputer),
    url::Host::Domain(_) => Some(HostClass::Public),
    url::Host::Ipv4(ip) => Some(ip_class(IpAddr::V4(ip))),
    url::Host::Ipv6(ip) => Some(ip_class(IpAddr::V6(ip))),
  }
}

fn ip_class(ip: IpAddr) -> HostClass {
  let ip = match ip {
    IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
    IpAddr::V4(_) => ip,
  };
  if ip.is_loopback() {
    return HostClass::ThisComputer;
  }
  let is_local = match ip {
    IpAddr::V4(v4) => {
      let [first, second, ..] = v4.octets();
      // 100.64.0.0/10: carrier-grade NAT, shared address space (RFC 6598).
      let is_shared = first == 100 && (second & 0b1100_0000) == 64;
      v4.is_private() || v4.is_link_local() || v4.is_unspecified() || v4.is_broadcast() || is_shared
    },
    IpAddr::V6(v6) => {
      let first = v6.segments()[0];
      // fc00::/7 unique local, fe80::/10 link-local.
      v6.is_unspecified() || (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80
    },
  };
  if is_local {
    HostClass::LocalNetwork
  } else {
    HostClass::Public
  }
}

/// The client for [`StorageReach::PublicOnly`]: like the shell's (no redirects), but names only
/// resolve to public addresses. A system proxy, where one is set, resolves names itself.
fn public_client() -> Result<&'static reqwest::Client, AuthError> {
  static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
  if let Some(client) = CLIENT.get() {
    return Ok(client);
  }
  let client = reqwest::Client::builder()
    .redirect(reqwest::redirect::Policy::none())
    .dns_resolver(Arc::new(PublicAddresses))
    .build()
    .map_err(|error| AuthError::Unsupported(error.to_string()))?;
  Ok(CLIENT.get_or_init(|| client))
}

/// Resolves names with the system resolver and drops every address that isn't public.
struct PublicAddresses;

impl reqwest::dns::Resolve for PublicAddresses {
  fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
    let host = name.as_str().to_owned();
    Box::pin(async move {
      let public: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
        .await?
        .filter(|address| ip_class(address.ip()) == HostClass::Public)
        .collect();
      if public.is_empty() {
        return Err(format!("{host} has no public address").into());
      }
      Ok(Box::new(public.into_iter()) as reqwest::dns::Addrs)
    })
  }
}

/// `GET url` without credentials (the URL is the credential); `(status, body)`. Fails once the body
/// grows past `max_bytes`, announced or not, or when it is not of `content_type`. Redirects are
/// not followed (the client's policy).
async fn get_object(
  http: &reqwest::Client,
  url: reqwest::Url,
  max_bytes: u64,
  content_type: &str,
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
  let served = response
    .headers()
    .get(reqwest::header::CONTENT_TYPE)
    .and_then(|value| value.to_str().ok())
    .unwrap_or("");
  if !is_same_media_type(served, content_type) {
    return Err(AuthError::InvalidResponse(format!(
      "storage served {served:?} instead of {content_type}"
    )));
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

/// `image/png; charset=binary` is `image/png`; case doesn't matter.
fn is_same_media_type(served: &str, expected: &str) -> bool {
  let essence = served.split(';').next().unwrap_or("").trim();
  essence.eq_ignore_ascii_case(expected)
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

/// The error without its URL: a presigned URL's query is its signature, so it must not reach
/// messages, toasts or logs.
fn network(error: reqwest::Error) -> AuthError {
  AuthError::Network(error.without_url().to_string())
}

#[cfg(test)]
mod tests {
  use tokio::io::{AsyncReadExt, AsyncWriteExt};
  use tokio::net::TcpListener;

  use super::*;
  use crate::auth::store::MemoryStore;

  const PNG: &str = "image/png";

  mod presigned_urls {
    use super::*;

    #[test]
    fn https_to_a_public_host_is_taken() {
      let url = presigned_url(
        "https://s3.example.com/bucket/a?X-Amz-Signature=1",
        StorageReach::PublicOnly,
      );
      assert_eq!(url.unwrap().host_str(), Some("s3.example.com"));
      assert!(presigned_url("https://203.0.113.7/a", StorageReach::PublicOnly).is_ok());
    }

    #[test]
    fn a_public_server_never_reaches_this_computer_or_its_network() {
      for raw in [
        "http://127.0.0.1:9000/a",
        "http://localhost:9000/a",
        "http://192.168.1.20:9000/a",
        "http://s3.example.com/a",
        "https://127.0.0.1/a",
        "https://localhost/a",
        "https://[::1]/a",
        "https://10.0.0.5/a",
        "https://172.16.3.4/a",
        "https://192.168.1.20/a",
        "https://169.254.169.254/latest/meta-data",
        "https://100.64.0.1/a",
        "https://0.0.0.0/a",
        "https://[fd00::1]/a",
        "https://[fe80::1]/a",
        "https://[::ffff:10.0.0.5]/a",
        "https://[::]/a",
      ] {
        assert!(presigned_url(raw, StorageReach::PublicOnly).is_err(), "{raw}");
      }
    }

    #[test]
    fn a_server_on_this_computer_may_use_storage_on_it_over_http() {
      for local in [
        "http://127.0.0.1:9000/a",
        "http://[::1]:9000/a",
        "http://localhost:9000/a",
        "https://192.168.1.20/a",
      ] {
        assert!(
          presigned_url(local, StorageReach::ThisComputer).is_ok(),
          "{local}"
        );
      }
      assert!(presigned_url("http://192.168.1.20:9000/a", StorageReach::ThisComputer).is_err());
      assert!(presigned_url("http://s3.example.com/a", StorageReach::ThisComputer).is_err());
      assert!(presigned_url("http://192.168.1.20:9000/a", StorageReach::Anywhere).is_ok());
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
        assert!(presigned_url(raw, StorageReach::Anywhere).is_err(), "{raw}");
      }
    }

    #[tokio::test]
    async fn a_public_name_resolving_to_this_computer_is_not_connected_to() {
      let url = serve_once("200 OK", Some(4), b"abcd".to_vec()).await;
      let port = url.port().unwrap();
      let by_name = reqwest::Url::parse(&format!("http://localhost:{port}/object")).unwrap();
      assert!(matches!(
        get_object(public_client().unwrap(), by_name, 4, PNG).await,
        Err(AuthError::Network(_))
      ));
    }
  }

  mod limits {
    use super::*;

    #[tokio::test]
    async fn a_picture_within_the_limit_is_read() {
      let url = serve_once("200 OK", Some(4), b"abcd".to_vec()).await;
      let (status, body) = get_object(&client(), url, 4, PNG).await.unwrap();
      assert_eq!((status, body), (200, b"abcd".to_vec()));
    }

    #[tokio::test]
    async fn an_announced_length_above_the_limit_is_refused() {
      let url = serve_once("200 OK", Some(5), b"abcde".to_vec()).await;
      assert!(matches!(
        get_object(&client(), url, 4, PNG).await,
        Err(AuthError::InvalidResponse(_))
      ));
    }

    #[tokio::test]
    async fn an_unannounced_body_stops_at_the_limit() {
      let url = serve_once("200 OK", None, vec![b'x'; 64 * 1024]).await;
      assert!(matches!(
        get_object(&client(), url, 1024, PNG).await,
        Err(AuthError::InvalidResponse(_))
      ));
    }

    #[tokio::test]
    async fn a_redirect_is_not_followed() {
      let url = serve_once("302 Found", Some(0), Vec::new()).await;
      let (status, body) = get_object(&client(), url, 4, PNG).await.unwrap();
      assert_eq!((status, body.len()), (302, 0));
    }

    #[tokio::test]
    async fn a_picture_of_another_type_is_refused() {
      let url = serve_once("200 OK", Some(4), b"abcd".to_vec()).await;
      assert!(matches!(
        get_object(&client(), url, 4, "image/jpeg").await,
        Err(AuthError::InvalidResponse(_))
      ));
      assert!(is_same_media_type("Image/PNG; charset=binary", PNG));
    }

    #[tokio::test]
    async fn network_errors_do_not_carry_the_signature() {
      let unreachable = reqwest::Url::parse("http://127.0.0.1:9/a?X-Amz-Signature=secret").unwrap();
      let Err(AuthError::Network(message)) = get_object(&client(), unreachable, 4, PNG).await else {
        panic!("a closed port is a network error");
      };
      assert!(!message.contains("secret"), "{message}");
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
      let mut head = format!(
        "HTTP/1.1 {status}\r\nConnection: close\r\nContent-Type: image/png\r\nLocation: https://elsewhere.example.com/\r\n"
      );
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
