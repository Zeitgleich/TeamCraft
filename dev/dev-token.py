#!/usr/bin/env python3
"""Prints an access token of a dev user from the local Keycloak, for trying the API with curl.

Runs the real authorization-code + PKCE flow against the public client "teamcraft" (the realm
disables the password grant), then prints the access token.

  ./dev/dev-token.py alice
  curl -H "Authorization: Bearer $(./dev/dev-token.py alice)" http://127.0.0.1:8081/api/v1/me
  ./dev/dev-token.py alice id      # the ID token instead (carries tc_identity_key once it is set)

Dev only: passwords are "<user>-dev-pass" (see dev/keycloak/teamcraft-dev-realm.json).
DEV_TOKEN_ISSUER points it at another realm with the same users, e.g. the deploy dry run's Keycloak.
"""
import base64
import hashlib
import html
import http.cookiejar
import json
import os
import re
import secrets
import sys
import urllib.error
import urllib.parse
import urllib.request

ISSUER = os.environ.get("DEV_TOKEN_ISSUER", "http://127.0.0.1:8080/realms/teamcraft-dev")
CLIENT_ID = "teamcraft"
REDIRECT_URI = "http://127.0.0.1:9/callback"  # never contacted; Keycloak ignores the loopback port


class NoRedirect(urllib.request.HTTPRedirectHandler):
  def redirect_request(self, *args, **kwargs):
    return None


def main() -> None:
  if len(sys.argv) not in (2, 3) or (len(sys.argv) == 3 and sys.argv[2] != "id"):
    sys.exit(__doc__)
  user = sys.argv[1]
  verifier = secrets.token_urlsafe(48)
  challenge = base64.urlsafe_b64encode(hashlib.sha256(verifier.encode()).digest()).rstrip(b"=").decode()
  # Keycloak marks its cookies Secure even over http; browsers accept that for loopback, urllib needs a hint.
  jar = http.cookiejar.CookieJar(http.cookiejar.DefaultCookiePolicy(secure_protocols=("http", "https")))
  opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(jar), NoRedirect)

  query = urllib.parse.urlencode({
    "client_id": CLIENT_ID, "response_type": "code", "scope": "openid", "redirect_uri": REDIRECT_URI,
    "code_challenge": challenge, "code_challenge_method": "S256", "state": "dev",
  })
  page = opener.open(f"{ISSUER}/protocol/openid-connect/auth?{query}").read().decode()
  match = re.search(r'<form[^>]*action="([^"]+)"', page)
  if not match:
    sys.exit("login form not found (is the realm imported?)")
  form = {"username": user, "password": f"{user}-dev-pass", "credentialId": ""}
  request = urllib.request.Request(html.unescape(match.group(1)), urllib.parse.urlencode(form).encode())
  try:
    opener.open(request)
    sys.exit("login failed (unknown user or wrong password)")
  except urllib.error.HTTPError as redirect:
    location = redirect.headers.get("Location", "")
    if redirect.code not in (301, 302, 303):
      sys.exit(f"login request failed: HTTP {redirect.code}")
  code = urllib.parse.parse_qs(urllib.parse.urlparse(location).query).get("code")
  if not code:
    sys.exit(f"no authorization code in redirect: {location}")

  token_request = urllib.request.Request(f"{ISSUER}/protocol/openid-connect/token", urllib.parse.urlencode({
    "grant_type": "authorization_code", "client_id": CLIENT_ID, "code": code[0],
    "redirect_uri": REDIRECT_URI, "code_verifier": verifier,
  }).encode())
  answer = json.load(urllib.request.urlopen(token_request))
  print(answer["id_token"] if len(sys.argv) == 3 else answer["access_token"])


if __name__ == "__main__":
  main()
