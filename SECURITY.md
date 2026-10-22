# Security policy

TeamCraft is an end-to-end encrypted chat whose server is assumed to be untrusted
([ARCHITECTURE.md](ARCHITECTURE.md) §2, [docs/e2ee.md](docs/e2ee.md)). Reports about anything that
lets the server, another member or an outsider read, forge or tamper with content are especially
welcome.

## Reporting a vulnerability

Please **do not** open a public issue. Report privately through GitHub's
[private vulnerability reporting](../../security/advisories/new) ("Report a vulnerability" on the
repository's Security tab).

Include what you found, how to reproduce it, the affected version or commit, and the impact you
expect. We confirm receipt within a few working days and keep you updated until a fix is released.
We credit reporters in the advisory unless you prefer otherwise.

## Supported versions

Until the first stable release, only the latest commit on `main` receives security fixes.

## Scope

In scope: the server, SFU, bridge, desktop/web/mobile clients, the cryptography in
`teamcraft-crypto` and the vendored `davey` patches.

Out of scope: vulnerabilities that require a compromised client device, denial of service through
sheer traffic volume, and issues in third-party services (OIDC provider, S3, SIP providers) that
TeamCraft does not control.

The internal review in [docs/security-review.md](docs/security-review.md) lists known findings and
their status.
