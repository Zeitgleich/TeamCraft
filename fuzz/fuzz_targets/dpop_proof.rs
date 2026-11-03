//! DPoP proofs as they arrive in the `DPoP` header or a WebSocket's first frame (RFC 9449). Garbage
//! must produce an error, never a panic, and nothing but a proof signed for exactly this request may
//! pass. The second half starts from a valid proof, because random bytes rarely get past the
//! base64url and JSON layers.
#![no_main]

use std::sync::LazyLock;

use libfuzzer_sys::fuzz_target;
use teamcraft_crypto::dpop::{
  DpopExpectation, DpopNonces, DpopProofRequest, DpopSigningKey, JwkThumbprint, PublicOrigin, verify_proof,
};

const NOW: i64 = 1_800_000_000;
const TOKEN: &str = "an.access.token";
const ORIGIN: &str = "https://chat.example.com";
const PATH: &str = "/api/v1/me";

static ORIGINS: LazyLock<Vec<PublicOrigin>> =
  LazyLock::new(|| vec![PublicOrigin::parse(ORIGIN).expect("a valid origin")]);
static KEY: LazyLock<DpopSigningKey> = LazyLock::new(DpopSigningKey::generate);
static VALID: LazyLock<String> = LazyLock::new(|| {
  KEY
    .sign_proof(&DpopProofRequest {
      method: "GET",
      target_uri: &format!("{ORIGIN}{PATH}"),
      issued_at_unix: NOW,
      maybe_access_token: Some(TOKEN),
      maybe_nonce: Some("nonce"),
    })
    .expect("a valid request")
    .as_str()
    .to_owned()
});

fn expected(maybe_bound_key: Option<&JwkThumbprint>) -> DpopExpectation<'_> {
  DpopExpectation {
    method: "GET",
    path: PATH,
    public_origins: &ORIGINS,
    now_unix: NOW,
    max_age_secs: 60,
    leeway_secs: 60,
    access_token: TOKEN,
    maybe_bound_key,
  }
}

fuzz_target!(|data: &[u8]| {
  let bound = KEY.thumbprint();

  if let Ok(text) = std::str::from_utf8(data) {
    let _ = verify_proof(text, &expected(None));
    let _ = verify_proof(text, &expected(Some(&bound)));
    let _ = JwkThumbprint::parse(text);
    let _ = PublicOrigin::parse(text);
    let _ = DpopNonces::new(NOW).accepts(text, NOW, 300);
  }

  // A valid proof with one byte changed: a signature can't be forged by chance, so whatever passes
  // must be the unchanged proof.
  let (position, replacement) = match data {
    [high, low, replacement, ..] => (usize::from(*high) << 8 | usize::from(*low), *replacement),
    _ => return,
  };
  let mut proof = VALID.clone().into_bytes();
  let position = position % proof.len();
  proof[position] = replacement;
  if let Ok(text) = std::str::from_utf8(&proof)
    && verify_proof(text, &expected(Some(&bound))).is_ok()
  {
    assert_eq!(text, VALID.as_str(), "a changed proof passed");
  }
});
