# Opening links in the installed app

Permalinks, invitation and meeting links are `https://` addresses of an instance. This document says
what happens when somebody with an installed app opens one, per platform, and what the platforms
can't do. Issue #125.

## What a link can do

One parser for every client: `teamcraft-client-api::app_link` (mobile: through `teamcraft-ffi`). It is
outside input, since any web page or app can fire a link, so:

- It is an **allow-list of places**: `/home`, `/s/{space}[/c/{channel}]`, `/c/{conversation}`,
  `/dm/{conversation}`, `/c/{c}/t/{t}`, `/c/{c}/m/{m}[/t/{t}]`, `/meeting/{id or token}`. Settings,
  administration and the API can't be linked to from outside.
- Only `https://` (plain `http` for a server on this device), `teamcraft://open/<host>/<path>`, and
  the permalinks inside messages (`teamcraft:/c/…`). Anything odd is refused, not repaired: `%`
  escapes, `@`, backslashes, `..`, non-ASCII, control characters, more than 2048 bytes. A query is
  dropped. Fuzz target: `fuzz/fuzz_targets/app_link.rs`.
- **A link names a place, it does nothing there.** A message opens; joining a call, knocking at a
  meeting's lobby or accepting anything takes a click. On the desktop a meeting link from outside
  waits with "Open the meeting" before it knocks, because the knock tells the hosts.
- **Another instance is never opened silently.** The app asks. It shows the end of the host that says
  who owns it (the registrable domain) first and large, then the whole host, wrapped; a host beyond
  what DNS allows (253 characters, 63 per label) is refused, so a long name can't push that end out
  of sight. Non-ASCII is refused and punycode stays punycode, so a look-alike shows as what it is.
  Without an account it offers to sign in there, through the usual
  check and confirmation of a server the user typed; with an account it offers the browser, because
  the apps hold one account at a time. A device locked by management gets the browser only. Several
  host names of one server (brands) count as different hosts here: the client can't tell.
- **A link can't swap a question.** One that arrives while a question is open is dropped, so what the
  user answers is what they read. On the desktop links bring the window forward at most every three
  seconds, so a page firing them in a loop can't keep taking the focus. A link naming a place on the
  active server still moves the app there each time: it asks nothing, and shows only what the user
  can already see.
- **Secrets stay in the web client.** A share link's key (`/f/{id}#key`) and a one-time guest token
  (`#invite=`) live in the fragment, which browsers never send to a server. The parser reads them as
  "web only": the app asks, then hands the address to the browser, and neither goes into a
  `teamcraft://` link (on Android and iOS another app can claim that scheme) or a log. `Debug` of a
  parsed link redacts them.

Without an app, nothing changes: the link opens the web client. Links are never logged.

## What each platform really does

| Platform      | `https://` link of an instance                                           | `teamcraft://open/…`                     |
|---------------|--------------------------------------------------------------------------|------------------------------------------|
| Android       | Opens the app **only for hosts the build lists**; others go to the browser | Opens the app, on every instance         |
| iOS           | Opens the app **only for hosts the build lists**; others go to the browser | Opens the app, on every instance         |
| Windows       | Browser: the system has no https handler for an unpackaged app           | Opens the app, also a running one        |
| macOS         | Browser: Universal Links would need a provisioning profile                | Opens the app, also a running one        |
| Linux         | Browser: no such concept                                                 | Opens the app, also a running one        |
| Web client    | –                                                                        | Offers it: the "Open in the app" button  |

### Self-hosted instances and the official apps

**A store app can't take over the links of a host it doesn't know.** Android App Links and iOS
Universal Links both start from a list of hosts fixed when the app is built (the Android intent filter,
the iOS associated domains), and only then ask that host's `/.well-known` file whether it agrees. The
file alone doesn't do it, and neither system has a wildcard for "any host". Dynamic rules on newer
Android only narrow hosts that are already listed. So an official app can't open `https://` links of an
instance that isn't in its list, whatever that instance serves.

So on those instances a tap on an `https://` link opens the web client, and the web client shows
**"Open in the app"**. That button is a `teamcraft://open/<host>/<path>` link. The page can't know
whether an app is installed, so it never redirects by itself; without the app the button does nothing
useful, and the reader can close it for good (a choice kept in this browser). It sits in the page's flow
(the banner row, the sign-in card), never above it, so it can't cover the composer's send or a call's
controls, and it isn't in a guest's running call. It isn't shown in an
installed web app, on the bare address and sign-in page, or on links that are web only.

What a self-hosted instance gets without any setup: the button, on all platforms. What takes more: an
own build of the apps that lists its host (`teamcraft.appLinkHosts` on Android, `APP_ENTITLEMENTS` and
`TEAMCRAFT_APP_LINK_DOMAIN` on iOS) and the identifiers of that build in the admin console.

## Server

`GET /.well-known/assetlinks.json` and `GET /.well-known/apple-app-site-association` are routes of the
server, so they don't depend on a static directory, and they win over a file of that name in it. They
answer on every brand's host (an unknown host gets the usual refusal), as JSON, without redirects,
cacheable for five minutes, with `Vary` on the host. What they say comes from **Administration →
Settings → "Links open in the apps"** (`app_links`; instance-wide):

| Setting                     | Default                              | Meaning                                       |
|-----------------------------|--------------------------------------|-----------------------------------------------|
| `is_enabled`                | on                                   | Off: both files answer 404                    |
| `android_package_names`     | the official Play and libre builds   | Who may open links on Android                 |
| `android_cert_fingerprints` | the official signing certificate     | SHA-256, upper-case hex pairs with colons     |
| `apple_team_id`             | empty until published (see below)    | Empty: Apple's file answers 404               |
| `apple_bundle_id`           | `de.zeitgleich.teamcraft`            | With the team, the app ID Apple's file names  |

The files claim the places above and nothing else. The bare address stays with the browser, because a
browser's sign-in comes back to it and must not end up in the app; an invitation mail therefore links
to `/home`. Apple's file also excludes `/f/*` and `#*invite=*`. Android's filter can't look at a
fragment, so a one-time link that reaches the app is handed on to the browser after asking.

## Clients

**Android.** `MainActivity` has an always-present filter for `teamcraft://open`, and, for the hosts of
`teamcraft.appLinkHosts` (comma-separated; none by default), an `autoVerify` filter generated by
`build.gradle.kts`. The release workflow reads the repository variable `TEAMCRAFT_APP_LINK_HOSTS`. "Open
in browser" names the browser explicitly, or a verified host would come straight back to the app.

**iOS.** `CFBundleURLTypes` has `teamcraft`, and `onOpenURL` takes both forms. Universal Links need
`APP_ENTITLEMENTS = TeamCraft/Resources/TeamCraft.AppLinks.entitlements`, a host in
`TEAMCRAFT_APP_LINK_DOMAIN` and the Associated Domains capability on the app ID, so they are off by
default. "Open in browser" uses Safari's view, which iOS doesn't send back to an associated app.

**Android and iOS, meeting links.** The mobile apps show the meetings the user is invited to but have
no lobby. A meeting link there asks: "My meetings" or "Open in browser". It can't open one specific
meeting yet.

**Desktop.** `tauri-plugin-deep-link` with `plugins.deep-link` in `tauri.conf.json`, which makes the
bundler register the scheme: the installers on Windows, `CFBundleURLTypes` on macOS, `MimeType` in the
`.desktop` entry of deb and rpm (`packaging/linux/TeamCraft.desktop.hbs`, Tauri's own template plus
`%u`, without which the link would not reach the app), the Flatpak's own entry. An AppImage has no
installed entry, so it registers itself with the user's desktop at start. A second launch hands its link
to the running instance through `tauri-plugin-single-instance` (e2ee.md §9.4). Links wait in the shell
(`deep_links.rs`, the newest eight) until the UI takes them, so one that started the app isn't lost
before the webview exists. The UI routes them through `Route::parse`, the web client's own routing; one
that arrives while signed out waits for the sign-in.

## Residual risk: a link can suggest a malicious server

Before this feature the victim had to type a server's address; now a link can supply it. An attacker's
server can copy another instance's sign-in settings, so the user signs in at their real identity
provider and the attacker's server receives a token it can try to relay to the real server (#120).
"Sign in there" therefore warns more than a typed address does, in the question and again on the
confirmation: the user didn't enter this server, and signing in gives it access to their account. The
warning about an identity provider outside the server's domain still applies. This reduces the risk
and doesn't remove it: somebody who trusts the wrong name still signs in. What ends it is binding tokens
to devices with DPoP (#120, e2ee.md §3.1.2) with the policy `required`; the default is `optional`.
Message content stays end-to-end encrypted either way.

## Open points

- `OFFICIAL_APPLE_TEAM_ID` (`teamcraft-proto`) is empty until the iOS app is published with the team
  known, so an instance's Apple file answers 404 until then or until an admin sets a team.
- The official hosts are not in the repository (no real names): set `TEAMCRAFT_APP_LINK_HOSTS` for the
  Android release; the iOS entitlements need the same decision.
- The apps hold one account at a time. With several accounts the foreign-instance question can offer
  to add one while signed in.
- Identifiers are per instance, not per brand. A brand with its own white-label app would need them
  per brand.
- macOS Universal Links for the desktop app, and a meeting lobby in the mobile apps.
