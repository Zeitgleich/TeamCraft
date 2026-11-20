"""The App Store Connect API client shared by the iOS release scripts (app-store-submit.py,
upload-screenshots.py): a short-lived ES256 token from the team's API key, JSON requests, and a
dry-run mode that sends only reads and prints every write instead.

Environment, the same API key as the TestFlight upload (archive-testflight.sh):
  ASC_KEY_ID, ASC_ISSUER_ID   the App Store Connect API key (role App Manager)
  ASC_KEY_PATH                its .p8 file

Needs python3 with `cryptography` (for the token), imported only when a token is made.
"""

import base64
import json
import os
import time
import urllib.error
import urllib.request
from pathlib import Path

API = "https://api.appstoreconnect.apple.com/v1"
TOKEN_LIFETIME_SECONDS = 15 * 60
REQUEST_TIMEOUT_SECONDS = 60
# Reads are retried on a server error or a dropped connection; writes are not (not idempotent).
READ_ATTEMPTS = 4
READ_RETRY_BASE_SECONDS = 5


class Client:
  """The App Store Connect API with a short-lived ES256 token from the API key.

  With `dry_run`, GET requests go out (they change nothing) and every POST, PATCH and DELETE is
  printed with its body instead; it answers with a placeholder object whose ID names the type."""

  def __init__(self, key_id: str, issuer_id: str, private_key_pem: bytes, dry_run: bool = False) -> None:
    from cryptography.hazmat.primitives.serialization import load_pem_private_key

    self.key_id = key_id
    self.issuer_id = issuer_id
    self.private_key = load_pem_private_key(private_key_pem, password=None)
    self.dry_run = dry_run
    self.token = ""
    self.token_expiry = 0.0

  @classmethod
  def from_env(cls, dry_run: bool = False) -> "Client":
    return cls(env("ASC_KEY_ID"), env("ASC_ISSUER_ID"), Path(env("ASC_KEY_PATH")).read_bytes(), dry_run)

  def get(self, path: str) -> dict:
    for attempt in range(1, READ_ATTEMPTS + 1):
      try:
        return self.request("GET", path)
      except ApiError as error:
        if not error.is_transient() or attempt == READ_ATTEMPTS:
          raise
        time.sleep(READ_RETRY_BASE_SECONDS * 2 ** (attempt - 1))
    raise AssertionError("unreachable")

  def post(self, path: str, body: dict) -> dict:
    return self.write("POST", path, body)

  def patch(self, path: str, body: dict) -> dict:
    return self.write("PATCH", path, body)

  def delete(self, path: str) -> dict:
    return self.write("DELETE", path)

  def write(self, method: str, path: str, body: dict | None = None) -> dict:
    if self.dry_run:
      print(f"[dry run] {method} {path}" + (f" {json.dumps(body, sort_keys=True)}" if body is not None else ""))
      kind = (body or {}).get("data", {}).get("type", "object")
      return {"data": {"id": f"dry-run-{kind}", "type": kind, "attributes": {}}}
    return self.request(method, path, body)

  def request(self, method: str, path: str, body: dict | None = None) -> dict:
    data = json.dumps(body).encode() if body is not None else None
    request = urllib.request.Request(API + path, data=data, method=method, headers={
      "Authorization": f"Bearer {self.bearer()}",
      "Content-Type": "application/json",
    })
    try:
      with urllib.request.urlopen(request, timeout=REQUEST_TIMEOUT_SECONDS) as response:
        payload = response.read()
    except urllib.error.HTTPError as error:
      raise ApiError(method, path, error.code, error.read().decode(errors="replace")) from error
    except (urllib.error.URLError, TimeoutError) as error:
      raise ApiError(method, path, None, str(error)) from error
    return json.loads(payload) if payload else {}

  def bearer(self) -> str:
    if time.time() < self.token_expiry - 60:
      return self.token
    from cryptography.hazmat.primitives import hashes
    from cryptography.hazmat.primitives.asymmetric import ec
    from cryptography.hazmat.primitives.asymmetric.utils import decode_dss_signature

    now = int(time.time())
    header = {"alg": "ES256", "kid": self.key_id, "typ": "JWT"}
    claims = {"iss": self.issuer_id, "iat": now, "exp": now + TOKEN_LIFETIME_SECONDS, "aud": "appstoreconnect-v1"}
    signing_input = f"{b64url(json.dumps(header).encode())}.{b64url(json.dumps(claims).encode())}"
    # A JWT's ES256 signature is r and s, 32 bytes each, not the DER the library returns.
    r, s = decode_dss_signature(self.private_key.sign(signing_input.encode(), ec.ECDSA(hashes.SHA256())))
    signature = r.to_bytes(32, "big") + s.to_bytes(32, "big")
    self.token = f"{signing_input}.{b64url(signature)}"
    self.token_expiry = now + TOKEN_LIFETIME_SECONDS
    return self.token


class ApiError(Exception):
  """A refused or failed request; `status` is None when no HTTP answer came back."""

  def __init__(self, method: str, path: str, status: int | None, detail: str) -> None:
    super().__init__(f"{method} {path}: {'HTTP ' + str(status) if status else 'no answer'}: {detail}")
    self.status = status

  def is_transient(self) -> bool:
    return self.status is None or self.status == 429 or self.status >= 500


def b64url(data: bytes) -> str:
  return base64.urlsafe_b64encode(data).rstrip(b"=").decode()


def env(name: str) -> str:
  value = os.environ.get(name)
  if not value:
    raise SystemExit(f"{name} is not set")
  return value
