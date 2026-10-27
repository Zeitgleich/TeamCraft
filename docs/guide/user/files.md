# Files

Attachments are encrypted on your device before upload: the server and the storage provider only
hold ciphertext. The file name and size travel inside the encrypted message.

## Send and receive

- **Attach** with **+** in the composer, drop files into it or paste an image. The upload is
  chunked and **resumes** after a lost connection; a progress bar shows it, and *Try again* appears
  if it fails. Files over the instance limit (100 MiB by default) are refused before upload.
- Images get a **thumbnail** made on your device and open in a **gallery** (previous / next).
  Other files show a **preview** where possible and a **Download** button; downloaded files are
  saved on this device.
- On **Android and iOS**, several pictures in one message show as an **album**: up to four tiles,
  "+N" on the last for the rest. A tap opens the viewer on that picture; swipe through the
  message's pictures, or switch to all pictures of the chat (the ends don't wrap around). Pinch or
  double-tap to zoom; sender and time are on top, save, share and forward at the bottom. The preview
  shows at once and the original loads with its progress. **Settings → Chats → Download original
  images automatically** (*Always*, *On Wi-Fi only* by default, *Never*) decides whether it loads by
  itself or waits for **Load original**; on mobile data the default waits.
- The **Files** tab of a conversation lists everything shared in it.
- **Link previews** are made by the sender's device, within the limits and domain list your admin set.

## Share links

**Create share link** on a file gives a link that lets someone outside TeamCraft download that one
file. The key stays in the part of the link after `#`, which browsers never send to the server.
Links expire (7 days by default, 30 at most) and can be limited to a number of downloads. Members may
create them; guests may not by default.

## Retention

Files follow the retention of their channel. When messages are deleted by retention, the encrypted
files go too. Pinned messages survive retention by default.
