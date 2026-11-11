#!/usr/bin/env python3
"""Prints an access token of a dev user from the identity provider, for trying the API with curl.

Runs the real authorization-code + PKCE flow against the public client "teamcraft" (the dev realm
disables the password grant), then prints the access token.

  ./dev/dev-token.py alice
  curl -H "Authorization: Bearer $(./dev/dev-token.py alice)" http://127.0.0.1:8081/api/v1/me
  ./dev/dev-token.py alice id      # the ID token instead (carries tc_identity_key once it is set)

Dev only: passwords are "<user>-dev-pass" (see dev/keycloak/teamcraft-dev-realm.json).
Environment, for other providers with the same flow (e.g. the deploy dry run's Keycloak, or the
demo instance's Dex, docs/operations.md "Demo instance"):

  DEV_TOKEN_ISSUER    issuer URL (default: the local dev realm); endpoints come from its discovery
  DEV_TOKEN_PASSWORD  the password of every user instead of "<user>-dev-pass"; one user's own
                      wins from DEV_TOKEN_PASSWORD_<USER> (the name before any "@", upper case,
                      other characters as "_": DEV_TOKEN_PASSWORD_REVIEWER_1)
  DEV_TOKEN_SCOPES    scopes to ask for (default "openid"; Dex needs "openid profile email groups")
  DEV_TOKEN_REDIRECT  redirect URI registered for the client (default http://127.0.0.1:9/callback)

The login page may be Keycloak's (fields username/password) or Dex's (login/password); redirects
before and after it are followed until the redirect URI receives the code.
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

ISSUER = os.environ.get("DEV_TOKEN_ISSUER", "http://127.0.0.1:8080/realms/teamcraft-dev").rstrip("/")
CLIENT_ID = "teamcraft"
# Never contacted; Keycloak ignores the loopback port, Dex needs it registered as is.
REDIRECT_URI = os.environ.get("DEV_TOKEN_REDIRECT", "http://127.0.0.1:9/callback")
SCOPES = os.environ.get("DEV_TOKEN_SCOPES", "openid")
# Login pages and approval screens before the code; more means the provider is looping.
MAX_HOPS = 10


class NoRedirect(urllib.request.HTTPRedirectHandler):
  def redirect_request(self, *args, **kwargs):
    return None


def main() -> None:
  if len(sys.argv) not in (2, 3) or (len(sys.argv) == 3 and sys.argv[2] != "id"):
    sys.exit(__doc__)
  user = sys.argv[1]
  per_user = "DEV_TOKEN_PASSWORD_" + re.sub(r"[^A-Z0-9]", "_", user.split("@")[0].upper())
  password = os.environ.get(per_user) or os.environ.get("DEV_TOKEN_PASSWORD", f"{user}-dev-pass")
  discovery = json.load(urllib.request.urlopen(f"{ISSUER}/.well-known/openid-configuration"))
  verifier = secrets.token_urlsafe(48)
  challenge = base64.urlsafe_b64encode(hashlib.sha256(verifier.encode()).digest()).rstrip(b"=").decode()
  # Keycloak marks its cookies Secure even over http; browsers accept that for loopback, urllib needs a hint.
  jar = http.cookiejar.CookieJar(http.cookiejar.DefaultCookiePolicy(secure_protocols=("http", "https")))
  opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(jar), NoRedirect)

  query = urllib.parse.urlencode({
    "client_id": CLIENT_ID, "response_type": "code", "scope": SCOPES, "redirect_uri": REDIRECT_URI,
    "code_challenge": challenge, "code_challenge_method": "S256", "state": "dev",
  })
  location = sign_in(opener, f"{discovery['authorization_endpoint']}?{query}", user, password)
  code = urllib.parse.parse_qs(urllib.parse.urlparse(location).query).get("code")
  if not code:
    sys.exit(f"no authorization code in redirect: {location}")

  token_request = urllib.request.Request(discovery["token_endpoint"], urllib.parse.urlencode({
    "grant_type": "authorization_code", "client_id": CLIENT_ID, "code": code[0],
    "redirect_uri": REDIRECT_URI, "code_verifier": verifier,
  }).encode())
  answer = json.load(urllib.request.urlopen(token_request))
  print(answer["id_token"] if len(sys.argv) == 3 else answer["access_token"])


def sign_in(opener, url: str, user: str, password: str) -> str:
  """Follows the provider's pages from `url` until it redirects to the redirect URI; returns it."""
  request = urllib.request.Request(url)
  has_posted = False
  for _ in range(MAX_HOPS):
    try:
      page = opener.open(request).read().decode()
    except urllib.error.HTTPError as redirect:
      if redirect.code not in (301, 302, 303, 307):
        sys.exit(f"sign-in request failed: HTTP {redirect.code}")
      location = urllib.parse.urljoin(request.full_url, redirect.headers.get("Location", ""))
      if location.startswith(REDIRECT_URI):
        return location
      request = urllib.request.Request(location)
      continue
    if has_posted:
      sys.exit("login failed (unknown user or wrong password)")
    request = login_request(request.full_url, page, user, password)
    has_posted = True
  sys.exit("sign-in did not finish (too many redirects)")


def login_request(page_url: str, page: str, user: str, password: str) -> urllib.request.Request:
  match = re.search(r'<form[^>]*action="([^"]+)"', page)
  if not match:
    sys.exit("login form not found (is the realm imported, or the user set up?)")
  # Keycloak's submit button is named "login" too: its user field decides.
  user_field = "username" if re.search(r'name="username"', page) else "login"
  form = {user_field: user, "password": password}
  if user_field == "username":
    form["credentialId"] = ""
  action = urllib.parse.urljoin(page_url, html.unescape(match.group(1)))
  return urllib.request.Request(action, urllib.parse.urlencode(form).encode())


if __name__ == "__main__":
  main()
