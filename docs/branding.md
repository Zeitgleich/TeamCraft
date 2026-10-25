# Branding: several brands on one server

One TeamCraft instance is one workspace: everyone can chat with everyone. Several affiliated companies
can still share that instance and keep their own identity: the server answers under several host names
and each one shows its own name, logo, colors, login text, links and e-mail sender. A brand is
presentation only. It never changes who can see what; permissions, spaces and encryption are the same
for every brand.

Related: [PLAN.md](../PLAN.md) §1 and §11, [admin.md](admin.md), [notifications.md](notifications.md),
[meetings.md](meetings.md).

## 1. Concepts

| Term          | Meaning                                                                                      |
|---------------|----------------------------------------------------------------------------------------------|
| Brand         | A named set of presentation settings with one or more host names. Set by admins, not users.  |
| Default brand | Exactly one brand is the default. Used for unknown hosts (if allowed), tools and fallbacks.  |
| Request brand | The brand whose host name the request arrived on (`Host` header, validated).                 |
| Home brand    | The brand of a user's e-mail domain (`email_domains`), else the default brand.               |
| Organization  | Optional label on a user ("Brand A GmbH"), from an OIDC claim. Shown as a badge, filterable. |

Brand and organization are independent. A user with organization "Brand A" can sign in through the
domain of Brand B; the UI then looks like Brand B.

## 2. Server configuration

`[[brands]]` is a list in the TOML file (`teamcraft.example.toml` documents every key). Without any
entry the server runs as before with one built-in default brand named "TeamCraft".

```toml
[branding]
assets_dir = "/etc/teamcraft/brands"   # brand files are read from here, never from elsewhere
unknown_host = "reject"                # "reject" (default) or "default" (see §3)
trusted_proxies = []                   # proxy addresses whose X-Forwarded-Host replaces Host
served_hostnames = []                  # further host names the proxy serves, for console brands (§2.1)

[[brands]]
id = "brand-a"                         # [a-z0-9-], 1..=32, stable; used in URLs and by clients
is_default = true
hostnames = ["chat.brand-a.example.com", "chat.brand-a.example.org"]
display_name = "Brand A"
organization_name = "Brand A Ltd."     # optional, shown in the footer of the sign-in page
email_domains = ["brand-a.example.com"] # users with such an address have this home brand
logo = "brand-a/logo.svg"              # file names below assets_dir; *_dark optional
logo_dark = "brand-a/logo-dark.svg"
icon = "brand-a/icon.png"              # square, app and notification icon
icon_dark = "brand-a/icon-dark.png"
favicon = "brand-a/favicon.png"
favicon_dark = "brand-a/favicon-dark.png"
support_url = "https://support.brand-a.example.com"
privacy_url = "https://brand-a.example.com/privacy"
terms_url = "https://brand-a.example.com/terms"
imprint_url = "https://brand-a.example.com/imprint"
download_url = "https://brand-a.example.com/apps"   # where the apps are; default: the project's download page
connect_origins = []                   # extra CSP connect-src origins of this brand only
# content_security_policy = "..."      # replaces [web].content_security_policy for this brand

[brands.login_title]                   # localized text: language code -> text, "en" is the fallback
en = "Welcome to Brand A"
de = "Willkommen bei Brand A"
[brands.login_text]
en = "Sign in with your company account."

[brands.colors.light]                  # design tokens, see §4; every key optional
accent = "#0b7f78"
accent_strong = "#096c66"
on_accent = "#ffffff"
accent_soft = "#dff3f1"
sidebar = "#1a2434"
[brands.colors.dark]
accent = "#35c6ba"

[brands.mail]
sender_name = "Brand A Team"           # display name of the From header
from_address = "chat@brand-a.example.com" # optional, else [mail.smtp].from_address
imip_organizer_name = "Brand A"        # meetings: ORGANIZER CN when the brand sends invitations
invite_subject = "..."                 # optional, same placeholders as [mail]; else the global text
invite_body = "..."
```

Validation at startup (the server refuses to start otherwise):

- exactly one `is_default = true` when `[[brands]]` is non-empty; ids unique and well-formed;
- host names unique across brands, lower-case ASCII (IDN as punycode), no port, no scheme, no path;
  IP literals are not allowed (a brand needs a name a certificate can cover); `localhost` is allowed;
- colors are `#rgb` or `#rrggbb` hex values (nothing else is ever emitted into CSS, §4);
- asset names are relative, without `..`, and the files exist and are regular files below
  `assets_dir`; size limit per file (`[branding].max_asset_bytes`, default 512 KiB);
- URLs are `https://` (or `http://` for localhost);
- `connect_origins` are origins as in `[web]`.

### 2.1 Brands in the admin console

Admins manage brands in Administration → Brands ([admin.md](admin.md) §6.1; API
`/api/v1/admin/brands`). The configuration file is the seed:

- `[[brands]]` are shown as "from configuration file" and are read-only. "Take over into the database"
  copies one, with its images, into the database (`brands`, `brand_assets`); the database brand with
  the same id then replaces it. Deleting that database brand brings the configuration brand back.
- New brands are stored in the database; their images go to the object storage (`[storage]`), at
  most `max_asset_bytes` each, PNG, JPEG, WebP, ICO or SVG (checked by their first bytes, and they must
  match the declared type).
- A database brand may only use **served host names**: `[branding].served_hostnames` plus the host
  names of `[[brands]]`. The proxy and TLS layer must know a host name before a brand can use it, so
  adding one is a deployment change (the playbook's served host names), assigning it is a console
  change. The console shows every served host name and the brand it selects.
- The default: a database brand that says it is the default takes that role from the configuration's.
  Without configured brands and without a database default, the built-in brand stays the default.
- Content security policies and connect origins per brand stay in the configuration file
  ([admin.md](admin.md) §6.4); a taken-over brand keeps the ones it had.
- Every change builds the complete set of brands and checks it as at startup (one default, unique
  ids and host names, hex colors, https links) before anything is stored; a change that does not fit
  is refused as a whole. At startup a stored brand loses host names the server no longer serves, with
  a warning.

## 3. Choosing the brand of a request

Only host names from the configuration select a brand. Nothing else is ever used: not a query
parameter, not a cookie, not a header other than `Host` (and `X-Forwarded-Host` if
`X-Forwarded-Host` from a peer listed in `trusted_proxies`, because that proxy sets it).

1. Read the host: header value, lower-cased, trailing dot and port removed. An invalid or missing host
   counts as unknown.
2. Exact match against the configured list. No wildcard and no suffix matching (`evil-chat.brand-a.example.com`
   or `chat.brand-a.example.com.evil.example` must not match).
3. Match: that brand. No match: `unknown_host = "reject"` (the default) answers `421 Misdirected Request` with
   error code `unknown_host` for every route except `/healthz`, `/readyz`, `/api/internal/*` and the
   back-channel logout (probes and the identity provider reach the server by IP or internal names).
   Trade-off: the server cannot be opened by IP or an internal name, and a proxy that rewrites `Host`
   must pass the brand host name. `unknown_host = "default"` serves the default brand instead, marking
   the response so that nothing host-derived is reflected (links use the default brand's first host name,
   never the received host). Without any brand host name (no `[[brands]]`, or a default brand without
   hosts) there is nothing to match and every host is served.

The brand description sent to the client contains configured values only. The received host name is
never copied into a response, a link, an e-mail or a redirect.

## 4. HTTP surface

`GET /api/v1/brand`, no credentials, rate limited like the other public endpoints, answers the brand
of the request host:

```json
{
  "id": "brand-a",
  "display_name": "Brand A",
  "organization_name": "Brand A Ltd.",
  "origin": "https://chat.brand-a.example.com",
  "assets": { "logo": "/api/v1/brand/assets/logo", "logo_dark": "...", "icon": "...", "favicon": "..." },
  "colors": { "light": { "accent": "#0b7f78" }, "dark": { "accent": "#35c6ba" } },
  "login_title": { "en": "Welcome to Brand A", "de": "Willkommen bei Brand A" },
  "login_text": { "en": "..." },
  "links": { "support": "...", "privacy": "...", "terms": "...", "imprint": "...", "download": "..." }
}
```

- Asset URLs are relative; asset bytes come from `GET /api/v1/brand/assets/{slot}` (slot = `logo`, `logo_dark`,
  `icon`, `icon_dark`, `favicon`, `favicon_dark`) for the request brand. Slots are an enum, not file
  names: no path from the client ever reaches the file system. Content type from the extension
  (svg, png, jpeg, webp, ico); SVG is served with the API's `Content-Security-Policy: default-src 'none'` and `nosniff`,
  so it cannot run script when opened as a page. Cache: `public, max-age=300` plus an `ETag`; `Vary: Host`.
- `GET /api/v1/brand/theme.css` returns the brand's colors as CSS custom properties (light, dark by
  `prefers-color-scheme` and `[data-theme]` like `main.css`). It exists because the CSP forbids inline
  styles: the web shell links it before the first paint, so there is no flash of the default colors.
  Only hex colors that passed validation are written; the CSS contains no other configured text.
- `GET /api/v1/brand/manifest.webmanifest` is the web app manifest, so the web client can be
  installed (add to home screen): the brand's display name, `display: standalone`, its `chrome` color
  (light) as theme and splash color, and its `icon` (the PNG's own size, `any` for SVG). A brand
  without an icon gets the client's own icons (`/icons/`, a maskable one among them). The manifest
  contains no host name; start URL and scope are `/`.
- The unauthenticated responses are the same for every caller of one host, `Vary: Host`, cacheable for
  five minutes. They count against the per-address request limit but not against the stricter limit of
  the credential-less endpoints, because one page load makes several of these requests.
- `GET /api/v1/brands` (signed in; empty for guests, like the directory) lists `id`, `display_name` and colors of all brands, default first,
  without host names. It serves the space brand tag (§7).

Color keys map to the UI tokens:

| Key               | CSS token(s)          |
|-------------------|-----------------------|
| `accent`          | `--accent`, `--focus` |
| `accent_strong`   | `--accent-strong`     |
| `on_accent`       | `--on-accent`         |
| `accent_soft`     | `--accent-soft`       |
| `sidebar`         | `--sb-bg`             |
| `sidebar_current` | `--sb-current`        |
| `chrome`          | `--chrome`            |

Other tokens (danger, presence colors, text) are not brandable: status colors keep their meaning.

### Content-Security-Policy per brand

`web` picks the policy by the request brand: the brand's `content_security_policy` if set, else the
global one with `{connect_src}` extended by the brand's `connect_origins`. Because `'self'` is the
request origin, each brand host automatically allows only itself. The brand-specific policy is
validated like the global one.

## 5. Clients

- **Web**: served from the brand host. `index.html` links `/api/v1/brand/theme.css` after the app's own
  stylesheet and `/api/v1/brand/manifest.webmanifest` as its manifest; `<meta name="theme-color">`
  follows the top bar's color of the current theme, then the app fetches `/api/v1/brand` (same origin) to render logo, title, login text and
  links; the answer is cached in `localStorage` keyed by server URL, so the next start paints
  immediately and refreshes in the background.
- **Desktop, Android, iOS**: the app asks the server URL it is configured with (any host name of a
  brand) for `GET <url>/api/v1/brand` without credentials (the desktop shell has a `public_get`
  command for it), caches the answer per server URL and applies it. A cached brand is only used for that
  exact URL. Brand data comes from the server the user chose; the app never changes the server URL
  based on brand data. `origin` is informational: a brand may have several host names, so it is not
  compared with the entered URL.
- **Checking**: all platforms run the same checks (`teamcraft-client-api::brand`): only known color
  keys with `#rgb`/`#rrggbb` values, only asset paths of the brand endpoint (made absolute on the
  server URL), only https links (http for localhost), bounded text lengths.
- **Applying**: CSS custom properties are set through the CSSOM (`style.setProperty`, allowed under the
  CSP); images through `<img src>`; the document title follows the brand ("Brand – TeamCraft"); the favicon stays the product's mark, with
  a red dot while something is unread, so the tab is recognisable. The effective
  theme (setting, else the system) picks the `*_dark` assets and the `dark` colors.

## 6. Things that use the brand of the recipient

The recipient's brand is the home brand (e-mail domain match, else the default brand). Where a request
exists (a user clicks something) the request brand is used instead.

| Surface                        | Brand used                                    | Effect                                                  |
|--------------------------------|-----------------------------------------------|---------------------------------------------------------|
| Invitation / activation e-mail | Recipient's domain brand, else inviter's brand | Sender name, optional From address, texts, link host    |
| Meeting invitations (iMIP)     | Organizer's home brand                        | ORGANIZER CN, From name, join link host                 |
| Push / notification text       | Recipient's home brand                        | Notification title/icon name (content stays encrypted)  |
| E-mail digests                 | Recipient's home brand                        | Sender, footer, links                                   |
| Share-link page                | Brand that created the link                   | Page title, logo; link only works on that brand's hosts |
| Guest meeting links            | Brand of the meeting (organizer's home brand) | Join page branding, link host                           |

Mail to a domain no brand lists goes out as the **inviter's home brand** (their e-mail domain, else the
default brand), never as the brand of the `Host` the request came in on: the host header must not choose
the identity of mail the server sends. Every generated link uses the first host name of the chosen brand, never the received host. Implemented
today: invitation and activation e-mail, the mention digest, meeting invitations (iMIP, join links,
CalDAV events) and share links. Push texts follow when they carry brand names.

## 7. Spaces and brands

A space can carry an optional brand tag (`brand_id`). It is a default theme hint: when a user opens a
space tagged with a brand, the UI may show that brand's accent color (from `GET /api/v1/brands`) on the
space's rail icon and header. It does not restrict membership or visibility and never changes the
signed-in brand. Admins set it with `PATCH /api/v1/spaces/{id}` (`brand_id`; an unknown id is rejected,
an empty string clears it). The tag is server-visible metadata (like the space's existence), not secret.

## 8. Share links

A share link records the brand that created it. `GET /api/v1/shares/{id}` (info) and `.../download`
answer `410 share_unavailable` (the same answer as for expired links) on a host that belongs to a
different brand or on an unknown host. The app shell served at `/f/{id}` leaks nothing by itself: it asks
the info endpoint and shows "not available". Reason: the link text a recipient sees names a host; if the
same path worked on every domain, a look-alike configuration or a mis-routed domain could present a file
under another company's name. The URL shown to the creator is `<brand origin>/f/<id>` (the brand's first
host name, else `[shares].public_base_url`). If the creating brand is later removed from the
configuration, its links belong to the default brand; links from before brands existed work on every host.

## 9. Organization

An optional label per user, taken from the identity provider.

- `[oidc] organization_claim = "organization"`: name of the access-token claim (a string, or a list whose
  first entry counts; at most 100 characters, no control characters). Unset: nobody has an
  organization. In Keycloak add a token mapper to the app client. **The claim must come from a source
  only administrators can change**: a group name or the Organizations feature of the realm, or a user
  attribute that users cannot edit (set the attribute's permissions to admin-only in the realm's user
  profile). A user-editable attribute would let anybody label themselves as another company.
- A value is ignored (not shortened) when it is blank, longer than 100 characters, or contains control
  characters or invisible formatting characters (zero-width spaces, bidi overrides and isolates, the byte
  order mark, tag characters), so a label cannot imitate another or reorder the text around it.
- Stored in `users.organization`, updated at every sign-in where the claim is configured (a token
  without the claim clears it). Not editable by the user: the provider is the source of truth.
- Shown as a badge next to the name in the member list, the directory, the profile card and the mention
  suggestions. The directory filters by organization (`GET /api/v1/users?organization=...`;
  `GET /api/v1/organizations` lists the existing ones). Guests get an empty list and, like
  everywhere, see only people they share a conversation with.
- It is display data. No permission and no brand depends on it, and it never appears in verification
  or safety-number screens: it says what the provider claims, not who a key belongs to.

## 10. OIDC and Caddy

- One Keycloak client `teamcraft` serves all brands. Its **valid redirect URIs** and **web origins**
  list every brand host (`https://chat.brand-a.example.com/*`, ...), and
  **post logout redirect URIs** likewise. A brand without an entry there cannot sign in; the error
  appears at the identity provider, not in TeamCraft.
- The token audience is unchanged, so no server change is needed for additional hosts.
- The login page of Keycloak can be themed per client; to theme it per brand, create one client per
  brand (`teamcraft-brand-a`). Not supported in v1: the server validates one `client_id` as the
  audience (open question, §12).
- Back-channel logout is a server-to-server call to one internal URL and is independent of brands.
- Reverse proxy (deploy repo): one site block (or a snippet applied to several host names) per brand
  host, all proxying to the same server, passing `Host` unchanged. Certificates for each host (Caddy
  automatic HTTPS). `trust_forwarded_for` stays as is; `trusted_proxies` stays empty with Caddy, which passes `Host` unchanged.

## 11. Security

- **There is no tenant isolation.** All brands share one database, one set of spaces, one directory and
  one encryption domain. A brand changes what a visitor sees, not what an authenticated user may do. Do not
  use brands to separate companies that must not see each other; use separate instances for that.

- Host allow-list: the only input to brand selection; exact matching; `unknown_host = "reject"` for
  deployments that want unknown hosts refused outright. Recommended with a proxy that only
  forwards configured hosts anyway.
- No reflection: no response contains the received host. Links in mail and API payloads use configured
  host names. Test: a request with `Host: evil.example` never yields `evil.example` in any body.
- No phishing aid: an administrator can only brand host names the deployment serves (§2.1); brands are
  changed by instance admins only, every change is in the audit log, there is no per-user or
  per-request brand selection, and the client never accepts a brand from a server other than the one
  in its URL bar / entered URL.
- Assets are configured files or images admins uploaded (type checked by content); CSS contains only
  validated hex values and fixed token names;
  SVG logos are shown via `<img>` only (scripts in SVG do not run there) and are served with a locked-down CSP.
- Per-brand CSP as in §4; share links are bound to the creating brand's hosts (§8).
- Session tokens and local sessions are not brand-scoped: signing in on one brand host does not sign you
  in on another (different origins), which is the expected browser behaviour.

## 12. Open questions (product decisions)

1. **Home brand source**: e-mail domain mapping (`email_domains`) is implemented. Alternatives: map by
   organization, or remember the host a user signed in with last. Which one should win?
2. **Per-brand Keycloak clients** (to theme the identity provider's login page per brand): needs the
   server to accept several audiences. Wanted for v1?
3. **Unknown host default**: shipped as `reject` (421; probes exempt). Switch to `default` if the server must also be reachable by IP or through a proxy that rewrites `Host`.
4. **Push notification branding** on iOS/Android depends on one app per store listing; per-brand apps are
   out of scope, only the in-app theme and the notification text name follow the brand.
5. **Space brand tag** is a theming hint only; should it also limit who can be invited from which domain?

## 13. What is not there yet

- The space settings dialog has no brand picker; the tag is set through the API.
- Push notification texts do not name a brand yet; when they do they take the recipient's home brand from
  `BrandRegistry::for_email`.
- Android and iOS show the brand on the sign-in screen only (tile, name, logo, login title and text, links;
  checked through the FFI's `login_brand`); colors and the in-app chrome don't follow it yet. The server
  URL is fixed per build; a server-URL screen needs a public way to learn the OIDC issuer. SVG logos
  aren't drawn on mobile (the name is shown instead).
- One OIDC client for all brands (§10).

## 14. The product mark

Without a server brand the apps show TeamCraft's own mark: three people around a speech bubble,
and the wordmark "TeamCraft". With a server brand the sign-in screen shows TeamCraft's tile and the
brand's beside it, like two app icons (the brand's `icon`, else its initial on its accent), then the
brand's logo or name (§5). The server's built-in brand (no brand configured) counts as no brand.

Loading screens always show the mark, animated (issue #463): the heads swirl in, the bodies rise,
the speech bubble pops and types. The web (`index.html`, `style/loader.css`) animates the inlined
mark in CSS before any script runs. It stays through the session check until the app's first view
is there; then `loader.js` flies the mark into the sign-in card's tile, or the screen crossfades into
the chat (no card in between). Android and iOS draw the same parts with
the same timing on a canvas (`LoadingMark.kt`, `LoadingMarkView.swift`); on Android 12+ the system
splash plays the assembly as an animated vector and the app takes over at its clock and place. iOS's
launch screen is the background only, so the assembly starts there. Reduced motion: no movement,
the mark fades in and its dots brighten in turn.

| File                                 | What it is                                                         |
|--------------------------------------|--------------------------------------------------------------------|
| `assets/brand/reference/wordmark.png`| The supplied wordmark, the input of `wordmark.py`                  |
| `assets/brand/mark.svg`              | The mark, constructed by `scripts/brand/mark.py`                   |
| `assets/brand/wordmark.svg`, `-dark` | The wordmark for light and dark backgrounds (`wordmark.py`)        |
| `assets/brand/icon.svg`              | The mark on its rounded backplate (desktop, web, Linux scalable)   |
| `assets/brand/glyph.svg`             | The small-size icon: the bubble on the brand blue (below 40 px)    |

`mark.py` builds the mark from exact geometry measured from the artwork (circles, edges on a 30°
grid, tangent fillets, the same clear space of 15 units around every head), so every curve is a
true arc. `wordmark.py` traces the letters and fits smooth Béziers, as the typeface isn't available
as a font. `icons.py` renders every platform icon from them: Tauri's PNGs, `.ico`, `.icns` and tray
icons; the web app's install icons, favicon and the image of the sign-in screen; the iOS icon
(light, dark, tinted); Android's adaptive icon layers, themed icon and notification icon.
`mark_parts.py` writes the mark's parts as code for the apps' loading screens and Android's splash
icon (the web's loading screen inlines the mark in `index.html`; update it by hand). The generated files are committed because the builds use them; change a master
or a script and run:

```sh
python3 scripts/brand/mark.py
python3 scripts/brand/wordmark.py   # numpy, scipy, Pillow, potrace
python3 scripts/brand/icons.py --contact-sheet /path/to/sheet.png   # rsvg-convert or ImageMagick 7
python3 scripts/brand/mark_parts.py
```

The product's accent colour is the mark's blue: `#005dfc` in the light theme (5.3:1 with white
text), `#5c9dff` with `#04122e` text in the dark theme (6.8:1). It is the default of `--accent`
(`style/main.css`) and of the Android and iOS themes; a brand's own colours override it (§2).
