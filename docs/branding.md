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
| Brand         | A named set of presentation settings with one or more host names. Configured, not user data. |
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
unknown_host = "default"               # "default" or "reject" (see §3)
trust_forwarded_host = false           # take X-Forwarded-Host instead of Host (proxy must set it)

[[brands]]
id = "brand-a"                         # [a-z0-9-], 1..=32, stable; used in URLs and by clients
is_default = true
hostnames = ["chat.brand-a.example.com", "chat.brand-a.example.org"]
display_name = "Brand A"
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

## 3. Choosing the brand of a request

Only host names from the configuration select a brand. Nothing else is ever used: not a query
parameter, not a cookie, not a header other than `Host` (and `X-Forwarded-Host` if
`trust_forwarded_host`, because the proxy sets it).

1. Read the host: header value, lower-cased, trailing dot and port removed. An invalid or missing host
   counts as unknown.
2. Exact match against the configured list. No wildcard and no suffix matching (`evil-chat.brand-a.example.com`
   or `chat.brand-a.example.com.evil.example` must not match).
3. Match: that brand. No match: `unknown_host = "default"` serves the default brand but **marks the
   response so that nothing host-derived is reflected** (links use the default brand's first host name,
   never the received host); `unknown_host = "reject"` answers `421 Misdirected Request` with error code
   `unknown_host` for every route except `/healthz`, `/readyz`, `/api/internal/*` and the back-channel
   logout (probes and the identity provider reach the server by IP or internal names).

The brand description sent to the client contains configured values only. The received host name is
never copied into a response, a link, an e-mail or a redirect.

## 4. HTTP surface

`GET /api/v1/brand`, no credentials, rate limited like the other public endpoints, answers the brand
of the request host:

```json
{
  "id": "brand-a",
  "display_name": "Brand A",
  "origin": "https://chat.brand-a.example.com",
  "assets": { "logo": "/api/v1/brand/assets/logo", "logo_dark": "...", "icon": "...", "favicon": "..." },
  "colors": { "light": { "accent": "#0b7f78" }, "dark": { "accent": "#35c6ba" } },
  "login_title": { "en": "Welcome to Brand A", "de": "Willkommen bei Brand A" },
  "login_text": { "en": "..." },
  "links": { "support": "...", "privacy": "...", "terms": "...", "imprint": "..." }
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
- The unauthenticated responses are the same for every caller of one host, `Vary: Host`, cacheable for
  five minutes. They count against the per-address request limit but not against the stricter limit of
  the credential-less endpoints, because one page load makes several of these requests.
- `GET /api/v1/brands` (signed in) lists `id`, `display_name` and colors of all brands, default first,
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
  stylesheet, then the app fetches `/api/v1/brand` (same origin) to render logo, title, login text and
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
  CSP); images through `<img src>`; the document title and favicon follow the brand. The effective
  theme (setting, else the system) picks the `*_dark` assets and the `dark` colors.

## 6. Things that use the brand of the recipient

The recipient's brand is the home brand (e-mail domain match, else the default brand). Where a request
exists (a user clicks something) the request brand is used instead.

| Surface                        | Brand used                                    | Effect                                                  |
|--------------------------------|-----------------------------------------------|---------------------------------------------------------|
| Invitation / activation e-mail | Recipient's domain brand, else request brand  | Sender name, optional From address, texts, link host    |
| Meeting invitations (iMIP)     | Organizer's home brand                        | ORGANIZER CN, From name, join link host                 |
| Push / notification text       | Recipient's home brand                        | Notification title/icon name (content stays encrypted)  |
| E-mail digests                 | Recipient's home brand                        | Sender, footer, links                                   |
| Share-link page                | Brand that created the link                   | Page title, logo; link only works on that brand's hosts |
| Guest meeting links            | Brand of the meeting (organizer's home brand) | Join page branding, link host                           |

Every generated link uses the first host name of the chosen brand, never the received host. Implemented
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
  organization. In Keycloak add a token mapper (user attribute, or group name) to the app client.
- Stored in `users.organization`, updated at every sign-in where the claim is configured (a token
  without the claim clears it). Not editable by the user: the provider is the source of truth.
- Shown as a badge next to the name in the member list, the directory, the profile card and the mention
  suggestions. The directory filters by organization (`GET /api/v1/users?organization=...`;
  `GET /api/v1/organizations` lists the existing ones). Guests get an empty list and, like
  everywhere, see only people they share a conversation with.
- It is display data. No permission and no brand depends on it.

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
  automatic HTTPS). `trust_forwarded_for` stays as is; `trust_forwarded_host` is not needed with Caddy.

## 11. Security

- Host allow-list: the only input to brand selection; exact matching; `unknown_host = "reject"` for
  deployments that want unknown hosts refused outright. Recommended with a proxy that only
  forwards configured hosts anyway.
- No reflection: no response contains the received host. Links in mail and API payloads use configured
  host names. Test: a request with `Host: evil.example` never yields `evil.example` in any body.
- No phishing aid: an administrator can only brand hosts they configure; there is no API to create or
  change a brand at run time, no per-user or per-request brand selection, and the client never
  accepts a brand from a server other than the one in its URL bar / entered URL.
- Assets are read-only configured files; CSS contains only validated hex values and fixed token names;
  SVG logos are shown via `<img>` only (scripts in SVG do not run there) and are served with a locked-down CSP.
- Per-brand CSP as in §4; share links are bound to the creating brand's hosts (§8).
- Session tokens and local sessions are not brand-scoped: signing in on one brand host does not sign you
  in on another (different origins), which is the expected browser behaviour.

## 12. Open questions (product decisions)

1. **Home brand source**: e-mail domain mapping (`email_domains`) is implemented. Alternatives: map by
   organization, or remember the host a user signed in with last. Which one should win?
2. **Per-brand Keycloak clients** (to theme the identity provider's login page per brand): needs the
   server to accept several audiences. Wanted for v1?
3. **Unknown host default**: `default` (serve default brand) or `reject` (421)? Shipped default is `default`.
4. **Push notification branding** on iOS/Android depends on one app per store listing; per-brand apps are
   out of scope, only the in-app theme and the notification text name follow the brand.
5. **Space brand tag** is a theming hint only; should it also limit who can be invited from which domain?

## 13. What is not there yet

- The space settings dialog has no brand picker; the tag is set through the API.
- Push notification texts do not name a brand yet; when they do they take the recipient's home brand from
  `BrandRegistry::for_email`.
- Android and iOS shells call `GET /api/v1/brand` and run `teamcraft-client-api::brand` once they get a
  server-URL screen; the server and the checks are ready.
- One OIDC client for all brands (§10).
