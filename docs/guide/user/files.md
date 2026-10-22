# Files

Attachments are encrypted on your device before upload: the server and the storage provider only
hold ciphertext. The file name and size travel inside the encrypted message.

## What works today

| Part                                                | Status                                            |
|-----------------------------------------------------|---------------------------------------------------|
| Encrypted upload and download (client core, server) | Built and tested                                  |
| Attach and download in the desktop app              | Coming soon: the composer lists names only        |
| Android                                             | Coming soon                                       |
| Share links (`/f/<id>` download page)               | Server page exists; creating links is coming soon |

Your admin has to configure object storage for attachments; without it the server answers *service
unavailable* ([Admin: configuration](../admin/configuration.md)). The default size limit is
100 MiB per attachment.

## Retention

Files follow the retention of their channel. When messages are deleted by retention, the encrypted
files go too. Pinned messages survive retention by default.
