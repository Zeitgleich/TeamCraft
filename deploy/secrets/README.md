Secrets of a deployment: one sops + age encrypted YAML file, `secrets.enc.yaml` next to your inventory
(README.md, "Your own inventory"), never in this repository. Expected keys:

| Key                           | Used as                                                                |
|-------------------------------|------------------------------------------------------------------------|
| `postgres_password`           | podman secret for `tc-db`; inside the database URL of `tc-server`      |
| `s3_access_key`               | podman secret for `tc-server`                                          |
| `s3_secret_key`               | podman secret for `tc-server`                                          |
| `sfu_registration_token`      | podman secret for `tc-server`, `tc-sfu`                                |
| `call_signing_key`            | podman secret (file) for `tc-server`: Ed25519 PEM, signs join tokens   |
| `call_signing_public_key`     | not secret; rendered for `tc-sfu`, which checks join tokens with it    |
| `meetings_guest_token_secret` | podman secret for `tc-server`: at least 32 random bytes                |
| `oidc_sync_client_secret`     | only with `teamcraft_oidc_sync_enabled`; podman secret for `tc-server` |
| `smtp_password`               | only with `teamcraft_smtp`; podman secret for `tc-server`              |
| `vapid_private_key`           | optional (Web Push); podman secret for `tc-server`                     |

Generate them:

```sh
openssl rand -hex 32                                   # postgres_password, sfu_registration_token
openssl rand -base64 48                                # meetings_guest_token_secret
openssl genpkey -algorithm ed25519 -out call.pem       # call_signing_key (file content)
openssl pkey -in call.pem -pubout                      # call_signing_public_key
openssl rand -base64 32 | tr '+/' '-_' | tr -d '='     # vapid_private_key
```

Throwaway test targets (`test/dry-run.sh`) use `teamcraft_secrets_source: file` with a generated
plain YAML file outside the repository instead; never commit such a file.
