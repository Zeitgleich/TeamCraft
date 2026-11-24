//! The messages between the page and its core's worker (`core_worker`, issue #2363): JSON strings,
//! since both ends are the same build (the worker runs the page's own module).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use teamcraft_client_api::failure::FailureKind;
use teamcraft_proto::{ErrorCode, UserId};

/// Messages to the worker (JSON strings; both ends are the same build).
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ToWorker {
  Start {
    user: UserId,
    server_url: String,
    maybe_token: Option<String>,
    maybe_id_token: Option<String>,
    is_bound: bool,
    is_visible: bool,
    was_store_ready: bool,
  },
  Call {
    id: u64,
    command: String,
    args: Value,
  },
  Token {
    token: String,
    maybe_id_token: Option<String>,
    is_bound: bool,
  },
  Visible {
    is_visible: bool,
  },
  /// The page is hidden, frozen or closing: save now.
  WriteNow,
}

/// Messages to the page.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ToPage {
  Started,
  Unsupported {
    reason: String,
  },
  /// Sent by `core-worker.js` when the module didn't instantiate.
  BootFailed {
    error: String,
  },
  Answer {
    id: u64,
    result: Result<Value, String>,
  },
  Changed {
    is_local: bool,
  },
  StoreBuild {
    maybe_percent: Option<u8>,
  },
  Failure {
    kind: FailureTag,
    detail: String,
  },
  StoreReady,
  WantsIdToken,
}

/// [`FailureKind`] as far as the core's background commands report it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FailureTag {
  Server(ErrorCode),
  Network,
  CaptivePortal,
  UntrustedCertificate,
  MustUpdate,
  Other,
}

impl From<FailureTag> for FailureKind {
  fn from(tag: FailureTag) -> Self {
    match tag {
      FailureTag::Server(code) => Self::Server(code),
      FailureTag::Network => Self::Network,
      FailureTag::CaptivePortal => Self::CaptivePortal,
      FailureTag::UntrustedCertificate => Self::UntrustedCertificate,
      FailureTag::MustUpdate => Self::MustUpdate,
      FailureTag::Other => Self::Other,
    }
  }
}

impl FailureTag {
  /// The tag of `kind`; kinds the core's background commands never report go as `Other`.
  pub(crate) fn of(kind: &FailureKind) -> Self {
    match kind {
      FailureKind::Server(code) => Self::Server(code.clone()),
      FailureKind::Network => Self::Network,
      FailureKind::CaptivePortal => Self::CaptivePortal,
      FailureKind::UntrustedCertificate => Self::UntrustedCertificate,
      FailureKind::MustUpdate => Self::MustUpdate,
      _ => Self::Other,
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn messages_to_the_worker_are_tagged_json() {
    let json = serde_json::to_value(ToWorker::Visible { is_visible: false }).unwrap();
    assert_eq!(
      json,
      serde_json::json!({ "type": "visible", "is_visible": false })
    );
    let json = serde_json::to_value(ToWorker::WriteNow).unwrap();
    assert_eq!(json, serde_json::json!({ "type": "write_now" }));
  }

  #[test]
  fn answers_carry_the_result() {
    let answer = ToPage::Answer {
      id: 7,
      result: Err("refused".to_owned()),
    };
    let json = serde_json::to_string(&answer).unwrap();
    let ToPage::Answer { id, result } = serde_json::from_str(&json).unwrap() else {
      panic!("not an answer: {json}");
    };
    assert_eq!((id, result), (7, Err("refused".to_owned())));
  }

  #[test]
  fn background_failures_keep_their_wording() {
    for kind in [
      FailureKind::Server(ErrorCode::from("channel_not_found".to_owned())),
      FailureKind::Network,
      FailureKind::CaptivePortal,
      FailureKind::UntrustedCertificate,
      FailureKind::MustUpdate,
      FailureKind::Other,
    ] {
      assert_eq!(FailureKind::from(FailureTag::of(&kind)), kind);
    }
  }
}
