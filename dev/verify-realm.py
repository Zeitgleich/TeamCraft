#!/usr/bin/env python3
"""Checks that the dev realm supports OIDC attestation of identity keys (e2ee.md §3.1).

Needs the running dev stack with a freshly imported realm. For user "alice" it verifies that

  * the public client's access token can use Keycloak's account API (audience "account"),
  * the user can write the profile attribute tc_identity_key and gets it back as an ID token claim,
  * the server's service account can read users but cannot change that attribute.

Exits non-zero with a message on the first failure. Resets the attribute at the end.
"""
import base64
import importlib.util
import json
import pathlib
import sys
import urllib.error
import urllib.parse
import urllib.request

BASE = "http://127.0.0.1:8080/realms/teamcraft-dev"
ADMIN = "http://127.0.0.1:8080/admin/realms/teamcraft-dev"

spec = importlib.util.spec_from_file_location("dev_token", pathlib.Path(__file__).with_name("dev-token.py"))
dev_token = importlib.util.module_from_spec(spec)
spec.loader.exec_module(dev_token)


def token(user: str, kind: str = "access") -> str:
  sys.argv = ["dev-token.py", user] + (["id"] if kind == "id" else [])
  import io, contextlib
  out = io.StringIO()
  with contextlib.redirect_stdout(out):
    dev_token.main()
  return out.getvalue().strip()


def claims(jwt: str) -> dict:
  payload = jwt.split(".")[1]
  return json.loads(base64.urlsafe_b64decode(payload + "=" * (-len(payload) % 4)))


def call(method: str, url: str, bearer: str, body=None):
  data = json.dumps(body).encode() if body is not None else None
  request = urllib.request.Request(url, data=data, method=method, headers={
    "Authorization": f"Bearer {bearer}", "Accept": "application/json", "Content-Type": "application/json"})
  try:
    with urllib.request.urlopen(request) as response:
      raw = response.read()
      return response.status, json.loads(raw) if raw else None
  except urllib.error.HTTPError as error:
    return error.code, None


def check(condition: bool, message: str) -> None:
  if not condition:
    sys.exit(f"FAILED: {message}")
  print(f"ok: {message}")


def set_attribute(bearer: str, value: list) -> None:
  status, account = call("GET", f"{BASE}/account", bearer)
  check(status == 200, "account API answers the public client's access token")
  account.pop("userProfileMetadata", None)
  account["attributes"] = {"tc_identity_key": value}
  status, _ = call("POST", f"{BASE}/account", bearer, account)
  check(status == 204, f"user writes tc_identity_key = {value}")


def main() -> None:
  access = token("alice")
  check("account" in (claims(access).get("aud") if isinstance(claims(access).get("aud"), list) else [claims(access).get("aud")]),
        "access token has the account audience")
  set_attribute(access, ["fp:verify-realm"])
  check(claims(token("alice", "id")).get("tc_identity_key") == "fp:verify-realm", "ID token carries tc_identity_key")

  form = urllib.parse.urlencode({"grant_type": "client_credentials", "client_id": "teamcraft-server",
                                 "client_secret": "teamcraft-server-dev-secret"}).encode()
  with urllib.request.urlopen(urllib.request.Request(f"{BASE}/protocol/openid-connect/token", form)) as response:
    service = json.load(response)["access_token"]
  status, users = call("GET", f"{ADMIN}/users?username=alice&exact=true", service)
  check(status == 200 and users, "service account reads users")
  status, _ = call("PUT", f"{ADMIN}/users/{users[0]['id']}", service, {"attributes": {"tc_identity_key": ["evil"]}})
  check(status == 403, "service account can't change tc_identity_key")
  set_attribute(access, [""])


if __name__ == "__main__":
  main()
