//! Local fakes of the push services (APNs, FCM with its OAuth endpoint, a UnifiedPush distributor).
//! They record what the server sends so tests can check the request format and decrypt the payload.
//! The keys below are throwaway test keys that protect nothing.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, Version};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};

pub const APNS_KEY_PEM: &str = r#"-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQg6G/tz4y1fJr77K7z
8ihxQPWlvTEJdzjz7EQQOhyZDCihRANCAATZcK4MntsHsGrLiiYY/R5u+PkPZpuN
dgo8LHX0AxF1j8QPNmXOWNMLAbz+fRtRJGmQiDQP92HZZ1d7Hu/IeczB
-----END PRIVATE KEY-----
"#;
pub const APNS_PUBLIC_PEM: &str = r#"-----BEGIN PUBLIC KEY-----
MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAE2XCuDJ7bB7Bqy4omGP0ebvj5D2ab
jXYKPCx19AMRdY/EDzZlzljTCwG8/n0bUSRpkIg0D/dh2WdXex7vyHnMwQ==
-----END PUBLIC KEY-----
"#;
pub const FCM_KEY_PEM: &str = r#"-----BEGIN PRIVATE KEY-----
MIIEugIBADANBgkqhkiG9w0BAQEFAASCBKQwggSgAgEAAoIBAQD0JVWJFW9yq1Gz
fmkmIqk36LWkRokAk/F/lvXWpGdQJlmUME+dAs/3iRnQhIWZSUqbEd5hw8e3puhh
k9XuqAGU91Qi+sFxvR8G3JfZRFnVz4bm4FpU+bCNjT3I0KCNiS6xSS9TBU72ekCL
mDYwHwYbVaMugoOIVW9Ay+f6mOEjRD6/XETxYjZo1Yvp7piSrV3Le0L4FVJUSA/p
qD+cL2oYODk28mSPAl4TDYF//m/NvnxsDeLJFm5pj9LAEzVDy0e8/rHpKh5sS3hL
qrmsDq3niOLIS6/5IGc9oRgcJOTG9d3rML6j0w0MEXx2dV2M2j8kkhMQ+q3aVGW+
B+FudPurAgMBAAECggEAQr6cIjRaefpNBSmXXGYYZeD4G3dvB9nqKSvffHUIi3hY
f/MvKKj2quDvzjoUfgHz4+3gIlEQHeyC1f5A+5QxjoSJG2D7FnmR7GHNH4gYmSO5
aQ/HkjiKpEALNoWWhusMeUodJnCqlCZ6087ibYhltn5UdKgYhmJ43jmAa1l1HxsF
5rHGOZaUhfbXr6Hay/ivYZR/hrS2rvR24MtXXJs2Lk4rrATaQYop/bYxrG5y7Hax
o2dOjFUPgwbMw3jIZT33ZW+VIXOeDFaU/Mn+VVReKYm2iX1t0B3zhoPIyT+mlNOk
7HSzw5MZFF99TvDUFDe/k5rV8GqN/A0gv5aZtR3iSQKBgQD+wgwg5f421N3gb/rB
3RTGyOXPaA3rqnaMD6TNHA/jHITTLhgIBchfjQZa+EagYYgmS60TKdWrMNMXSkE8
8ciocy2l2mNnjTpQzI+A+7HBVz6QDE14rK0e+eE8q3WgSWIt0y71hkgNO6SoIM5x
Vi4QVjSA+jwW1pdXFwb87pA3UwKBgQD1VgrK7dZu8vlA8uPLMf/KbGnQE+iL7OTJ
FTb5/fJZnfEVFmXrprgaqM2ytKr7MmC6nbtkyPH+BlSAKLgApnM8L0anozrxtkkn
RuGfayzX51+XLjdAW10eaC5Cmkst0ngFkPFt5MUCJpdI/nup+oGT0/QdHMY6gI3b
p1HrIFhXSQKBgHwzgJQ9bN2wk+eK0HbZYPq1EQ04DqlZdug77D2vM7OmwL7wVvj6
UMD80R9isZQZWLZK6LtSBwCqEg/lvL6Ipiox1Qxp11Kx0L95q3DXxMac0TyLMu3l
SZWzf3MDqJoXjM0Ii3laGSCSdmkdbWhl+pkuXfSPiuIbhrcH2/lOGXNPAn87apsy
imdkd3PmRuzkSTepzpfRzQCE4tqF2J137gjDq3y3nrUQXVA5JKisnxTJwWmryIeZ
4RC6dM4CO9pUTfual9iEwQOFhqAw4vnBojM6Fs8PziGTdI0Y0CJj/vo2HsLylifi
dYVBABejlf5dLE41fx+f7VgU0w2IJP2x9CNxAoGAfLnYPCBcWfkcXHE8iHdcNhQb
pESgv1wQ15KfjUKUPoMy5tvJ30kLQLGr3mQf/PL+HCuJ+2PAmSzNTM7Ex2qWvrX8
77tavsQ6GHicujPi8BoarQTXmb1WBr9Tva8EwJWOm9wxl0UBSwqgujQziwxaWmvs
Nb5WAVt7SVsbM7K2x9g=
-----END PRIVATE KEY-----
"#;
pub const FCM_PUBLIC_PEM: &str = r#"-----BEGIN PUBLIC KEY-----
MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEA9CVViRVvcqtRs35pJiKp
N+i1pEaJAJPxf5b11qRnUCZZlDBPnQLP94kZ0ISFmUlKmxHeYcPHt6boYZPV7qgB
lPdUIvrBcb0fBtyX2URZ1c+G5uBaVPmwjY09yNCgjYkusUkvUwVO9npAi5g2MB8G
G1WjLoKDiFVvQMvn+pjhI0Q+v1xE8WI2aNWL6e6Ykq1dy3tC+BVSVEgP6ag/nC9q
GDg5NvJkjwJeEw2Bf/5vzb58bA3iyRZuaY/SwBM1Q8tHvP6x6SoebEt4S6q5rA6t
54jiyEuv+SBnPaEYHCTkxvXd6zC+o9MNDBF8dnVdjNo/JJITEPqt2lRlvgfhbnT7
qwIDAQAB
-----END PUBLIC KEY-----
"#;

pub const APNS_KEY_ID: &str = "KEY1234567";
pub const APNS_TEAM_ID: &str = "TEAM123456";
pub const APNS_BUNDLE_ID: &str = "com.example.teamcraft";
pub const FCM_PROJECT: &str = "example-project";
pub const FCM_ACCESS_TOKEN: &str = "ya29.fake-access-token";
pub const FCM_CLIENT_EMAIL: &str = "push@example-project.iam.gserviceaccount.com";

/// One request a fake push service received.
#[derive(Debug, Clone)]
pub struct Received {
  pub version: Version,
  pub headers: HeaderMap,
  pub path: String,
  pub body: Vec<u8>,
}

impl Received {
  pub fn header(&self, name: &str) -> &str {
    self
      .headers
      .get(name)
      .and_then(|value| value.to_str().ok())
      .unwrap_or("")
  }

  pub fn json(&self) -> Value {
    serde_json::from_slice(&self.body).unwrap()
  }
}

/// A fake that answers every request with the same status (and optional JSON body) and records them.
#[derive(Clone)]
pub struct Fake {
  received: Arc<Mutex<Vec<Received>>>,
  reply: Arc<Mutex<(u16, Value)>>,
  token_requests: Arc<Mutex<Vec<Received>>>,
}

impl Fake {
  /// The reply for all following requests.
  pub fn set_reply(&self, status: u16, body: Value) {
    *self.reply.lock().unwrap() = (status, body);
  }

  pub fn requests(&self) -> Vec<Received> {
    self.received.lock().unwrap().clone()
  }

  /// Requests to the fake OAuth token endpoint (FCM only).
  pub fn token_requests(&self) -> Vec<Received> {
    self.token_requests.lock().unwrap().clone()
  }

  /// Waits for `count` push requests.
  pub async fn wait_for(&self, count: usize) -> Vec<Received> {
    for _ in 0..100 {
      if self.requests().len() >= count {
        break;
      }
      tokio::time::sleep(Duration::from_millis(50)).await;
    }
    self.requests()
  }

  fn new() -> Self {
    Self {
      received: Arc::default(),
      reply: Arc::new(Mutex::new((200, Value::Null))),
      token_requests: Arc::default(),
    }
  }

  async fn record(&self, request: axum::extract::Request) -> Received {
    let (parts, body) = request.into_parts();
    let body = axum::body::to_bytes(body, 1 << 20).await.unwrap_or_default();
    Received {
      version: parts.version,
      headers: parts.headers,
      path: parts.uri.path().to_owned(),
      body: body.to_vec(),
    }
  }

  fn answer(&self) -> Response {
    let (status, body) = self.reply.lock().unwrap().clone();
    let status = StatusCode::from_u16(status).unwrap();
    if body.is_null() {
      status.into_response()
    } else {
      (status, axum::Json(body)).into_response()
    }
  }
}

async fn serve(router: Router) -> String {
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let address = listener.local_addr().unwrap();
  tokio::spawn(async move {
    axum::serve(listener, router).await.unwrap();
  });
  format!("http://{address}")
}

/// A fake APNs endpoint. It speaks HTTP/2 without TLS, which the sender uses for `http://` URLs.
/// Returns the fake and its base URL.
pub async fn apns() -> (Fake, String) {
  let fake = Fake::new();
  let router = Router::new()
    .route(
      "/3/device/{token}",
      post(
        |State(fake): State<Fake>, Path(_token): Path<String>, request: axum::extract::Request| async move {
          let received = fake.record(request).await;
          fake.received.lock().unwrap().push(received);
          fake.answer()
        },
      ),
    )
    .with_state(fake.clone());
  (fake, serve(router).await)
}

/// A fake FCM endpoint with its OAuth token endpoint at `/token`. Returns the fake and its base URL.
pub async fn fcm() -> (Fake, String) {
  let fake = Fake::new();
  let router = Router::new()
    .route(
      "/token",
      post(
        |State(fake): State<Fake>, request: axum::extract::Request| async move {
          let received = fake.record(request).await;
          fake.token_requests.lock().unwrap().push(received);
          axum::Json(json!({"access_token": FCM_ACCESS_TOKEN, "expires_in": 3600, "token_type": "Bearer"}))
        },
      ),
    )
    .route(
      "/v1/projects/{project}/messages:send",
      post(
        |State(fake): State<Fake>, Path(_project): Path<String>, request: axum::extract::Request| async move {
          let received = fake.record(request).await;
          fake.received.lock().unwrap().push(received);
          fake.answer()
        },
      ),
    )
    .with_state(fake.clone());
  (fake, serve(router).await)
}

/// The service account key file the FCM sender is configured with, pointing at the fake.
pub fn fcm_service_account_json(base_url: &str) -> String {
  json!({
    "type": "service_account",
    "project_id": FCM_PROJECT,
    "private_key": FCM_KEY_PEM,
    "client_email": FCM_CLIENT_EMAIL,
    "token_uri": format!("{base_url}/token"),
  })
  .to_string()
}

/// A fake UnifiedPush distributor. Returns the fake and the endpoint a device registers.
pub async fn unified_push() -> (Fake, String) {
  let fake = Fake::new();
  let router = Router::new()
    .route(
      "/up/{id}",
      post(
        |State(fake): State<Fake>, Path(_id): Path<String>, request: axum::extract::Request| async move {
          let received = fake.record(request).await;
          fake.received.lock().unwrap().push(received);
          fake.answer()
        },
      ),
    )
    .with_state(fake.clone());
  let base = serve(router).await;
  (fake, format!("{base}/up/device-1"))
}

/// The sealed wake-up of an APNs or FCM payload (`w`, base64url without padding).
pub fn sealed_from(value: &str) -> Vec<u8> {
  URL_SAFE_NO_PAD.decode(value).unwrap()
}
