# France: cryptology declaration and App Store availability

TeamCraft's iOS app is end-to-end encrypted with algorithms the OS does not provide (MLS with
openmls, RustCrypto primitives). Apple therefore asks for the French cryptology declaration before the
app may be offered on the French App Store, and App Store Connect keeps France switched off until the
supplier has one. This page records the rule, the decision and the filing. Last checked 2026-11-21.

## Rule

| Source                                                  | What it says                                                                                                                              |
|---------------------------------------------------------|-------------------------------------------------------------------------------------------------------------------------------------------|
| LCEN (law 2004-575) arts. 30-31, decree 2007-663        | Using a cryptology means is free. Supplying it in France is subject to a declaration to ANSSI, bar exceptions                             |
| ANSSI, "démarches à accomplir" (cyber.gouv.fr)          | Supply and import in France: *déclaration auprès de l'ANSSI*; the duty is the supplier's or first importer's                              |
| ANSSI notice V3 (31 March 2026)                         | Since 31 March 2026 the declaration is filed online at demarche.numerique.gouv.fr (form "Déclaration relative à un moyen de cryptologie") |
| Apple, "Export compliance documentation for encryption" | Standard algorithms beyond the OS's need the French declaration, only if the app is on the French App Store                               |

What ANSSI sends back: an acknowledgement of receipt, then an *attestation de déclaration* with a file
number (a link to download it), by e-mail and on the platform. Apple wants that attestation.

Unknown from public sources (ask `controle@ssi.gouv.fr`, or a French lawyer, if it matters): whether a
messaging app falls under an exception of decree 2007-663, the processing time of the new online route
(the old postal one was about a month), and whether the product is classified "grand public".

## Decision

France stays unavailable until the attestation exists. Nothing else blocks it: App Store Connect lists
France as the only EU country not available. Offering the app in France without a declaration would be
supplying a means of cryptology unlawfully; withholding it for a documented legal reason is not the
unjustified geo-blocking the EU rules forbid. File the declaration, then switch France on.

## Filing (by the supplier: the legal entity that publishes the app)

On demarche.numerique.gouv.fr, "Déclaration relative à un moyen de cryptologie", "Pour vous":

1. Beneficiary: legal entity, administrative contact, technical contact.
2. The means: brand and name `TeamCraft Chat` (the name in the attestation, as on the App Store), the
   version, the market date, software; main function: information security (team messaging).
3. Cryptographic services: confidentiality, integrity, authentication, signature; protocols: MLS (RFC
   9420), HPKE (RFC 9180), TLS (provided by the OS), DAVE for calls.
4. Algorithms (docs/e2ee.md; each entry: algorithm, mode, key size, use):

   | Algorithm                                                 | Key / size                    | Use                                                          |
   |-----------------------------------------------------------|-------------------------------|--------------------------------------------------------------|
   | MLS `MLS_128_DHKEMX25519_CHACHA20POLY1305_SHA256_Ed25519` | X25519, 256-bit AEAD, SHA-256 | Group end-to-end encryption of messages and keys (RFC 9420)  |
   | ChaCha20-Poly1305, XChaCha20-Poly1305                     | 256-bit                       | Message and file content keys, device-to-device transfer     |
   | Ed25519                                                   | 256-bit                       | Signatures of device keys, messages and signed policy        |
   | X25519, HPKE (RFC 9180)                                   | 256-bit                       | Key agreement and sealing keys to devices                    |
   | HKDF-SHA256, HMAC, SHA-256                                | 256-bit                       | Key derivation, integrity, hashes                            |
   | Argon2id                                                  | 128 MiB, about 1 s            | Stretching recovery passphrases and share-link passwords     |
   | AES-GCM                                                   | 256-bit (128-bit in DAVE)     | Local database and media at rest; call media frames (DAVE)   |
   | ECDSA / ECDH P-256, RSA                                   | 256-bit, up to 4096-bit       | Request proofs (DPoP) and verifying device attestations only |

   Engineering to confirm the table against the release's dependency list before filing.
5. Attachments: commercial brochure (the store listing text), technical brochure (docs/e2ee.md,
   exported to PDF), the signed attestation (template inside the form), and for a first declaration
   the company's registry extract (under three months old) and a company presentation. User and admin
   guides are optional (docs/admin.md).

## Apple, after ANSSI answered

1. App Store Connect, *App Information*, *App Encryption Documentation*, *Upload* (Account Holder, Admin
   or App Manager): the attestation. Apple reviews it (about two business days) and returns a code.
2. Set `ITSAppUsesNonExemptEncryption` to `true` and `ITSEncryptionExportComplianceCode` to the code in
   `apps/ios/project.yml`, release a build.
3. *Pricing and Availability*, *Manage*: switch France on. It shows in the store within a day.

Until then the app answers `ITSAppUsesNonExemptEncryption: false` (docs/release.md, TestFlight step 7).
That answer is a stretch for an app with its own algorithms; with the documentation uploaded the plist
changes as in step 2, which also makes the answer true.
