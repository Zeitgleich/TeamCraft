//! Videos that play while they stream (#200, e2ee.md §4.5): the player of the web page and the
//! desktop app reads a file in ranges, as it would from a web server, and the core answers each
//! range from the encrypted blob with an HTTP `Range` request to the storage, decrypting only the
//! chunks the range covers ([`AttachmentRangeDecryptor`]). The page never holds the whole file, so
//! a video of any size plays, and seeking jumps to where the user wants without loading what lies
//! before it.
//!
//! The blob's hash is not checked first, as it is for a file opened whole: it covers all of the
//! blob. Every chunk is authenticated on its own instead (its position and whether it is the last
//! are part of its nonce, the file's key is its own), so the player gets nothing the sender didn't
//! encrypt, in the place the sender put it.
//!
//! Memory stays bounded: a few streams are open at once ([`MAX_OPEN_STREAMS`], the oldest is closed
//! for a new one), a read covers at most [`MAX_READ_BYTES`], and a few reads run at once
//! ([`MAX_RUNNING_READS`]); the others wait their turn.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;

use rand_core::{OsRng, RngCore};
use teamcraft_crypto::attachment::{AttachmentKey, AttachmentRangeDecryptor, HEADER_LEN};
use teamcraft_crypto::secret::{SecretBytes, SymmetricKey};

use crate::api::ServerApi;
use crate::client::Client;
use crate::store::{AttachmentRef, LocalStore};

/// Plaintext bytes one read answers at most: whole chunks of 64 KiB, about a second of a phone's
/// video. A player asks again for the rest.
pub const MAX_READ_BYTES: u64 = 2 * 1024 * 1024;
/// Streams open at once: the lightbox's video, the one in a row and a few the user just left.
pub const MAX_OPEN_STREAMS: usize = 4;
/// Reads that run at once, over all streams; a player that seeks a lot leaves the rest waiting.
pub const MAX_RUNNING_READS: usize = 4;
/// How long a download URL is used after it arrived, by the client's clock, which only measures the
/// span: the server signs for 15 minutes by default (`presign_expiry_secs`).
const URL_REUSE_MS: i64 = 5 * 60 * 1000;
const TOKEN_BYTES: usize = 16;

/// The open streams of a host, and the requests waiting for its loop.
#[derive(Default)]
pub struct MediaStreams {
  /// Least recently used first.
  open: Vec<OpenStream>,
  waiting: VecDeque<StreamRequest>,
  running_reads: Rc<Cell<usize>>,
}

/// Names one open stream in the URL the player loads (`http://127.0.0.1:<port>/<token>` on the desktop,
/// `/media-stream/<token>` in the browser). Random, so no other page can guess it, and no file's ID.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct MediaStreamToken(String);

/// A stream that opened: what the player needs before its first read.
#[derive(Debug)]
pub struct OpenedStream {
  pub token: MediaStreamToken,
  /// The plaintext length, from the blob (not the sender's word).
  pub size_bytes: u64,
  /// What the player is told the file is: one of [`streamable_type`]'s.
  pub media_type: &'static str,
}

/// The answer to one read: `bytes` start at `start`; empty when `start` is at or past the end.
pub struct StreamPart {
  pub start: u64,
  pub bytes: SecretBytes,
  pub total_bytes: u64,
  /// One of [`streamable_type`]'s, as the stream opened with it.
  pub media_type: &'static str,
}

pub type DeliverOpened = Box<dyn FnOnce(Result<OpenedStream, String>)>;
pub type DeliverPart = Box<dyn FnOnce(Result<StreamPart, String>)>;

/// A future the host's download loop runs beside everything else.
pub type StreamWork = Pin<Box<dyn Future<Output = ()>>>;

struct OpenStream {
  token: MediaStreamToken,
  state: Rc<RefCell<StreamState>>,
}

struct StreamState {
  reference: AttachmentRef,
  media_type: &'static str,
  maybe_location: Option<Location>,
  maybe_decryptor: Option<Rc<AttachmentRangeDecryptor>>,
}

/// Where the blob is, as the server last said.
#[derive(Clone)]
struct Location {
  url: String,
  ciphertext_len: u64,
  fetched_ms: i64,
}

enum StreamRequest {
  Open {
    reference: AttachmentRef,
    deliver: DeliverOpened,
  },
  Read {
    token: MediaStreamToken,
    start: u64,
    deliver: DeliverPart,
  },
}

/// The media type a file streams as: the videos browsers and webviews play, by the type's essence.
/// Anything else isn't streamed, so a file that claims another type is never handed to the
/// webview as a document.
pub fn streamable_type(media_type: &str) -> Option<&'static str> {
  let essence = media_type
    .split(';')
    .next()
    .unwrap_or_default()
    .trim()
    .to_ascii_lowercase();
  [
    "video/mp4",
    "video/webm",
    "video/quicktime",
    "video/ogg",
    "video/x-matroska",
    "video/3gpp",
  ]
  .into_iter()
  .find(|known| *known == essence)
}

impl MediaStreams {
  pub fn new() -> Self {
    Self::default()
  }

  /// Queues opening a stream of a file; `deliver` gets its token and length once the blob's header
  /// arrived.
  pub fn open(&mut self, reference: AttachmentRef, deliver: DeliverOpened) {
    self.waiting.push_back(StreamRequest::Open { reference, deliver });
  }

  /// Queues reading from `start` on; `deliver` gets up to [`MAX_READ_BYTES`].
  pub fn read(&mut self, token: MediaStreamToken, start: u64, deliver: DeliverPart) {
    self.waiting.push_back(StreamRequest::Read {
      token,
      start,
      deliver,
    });
  }

  /// Closes a stream the page no longer plays. Reads already running still answer.
  pub fn close(&mut self, token: &MediaStreamToken) {
    self.open.retain(|stream| stream.token != *token);
  }

  /// Whether a request waits for [`Self::start_due`].
  pub fn has_waiting(&self) -> bool {
    !self.waiting.is_empty()
  }

  /// Starts what waits: asks the server for download URLs (a short request each) and hands back the
  /// transfers for the host to run beside its loop.
  pub async fn start_due<A: ServerApi, S: LocalStore>(&mut self, client: &Client<A, S>) -> Vec<StreamWork> {
    let mut work = Vec::new();
    let mut postponed = VecDeque::new();
    while let Some(request) = self.waiting.pop_front() {
      match request {
        StreamRequest::Open { reference, deliver } => {
          if let Some(started) = self.start_open(client, reference, deliver).await {
            work.push(started);
          }
        },
        StreamRequest::Read {
          token,
          start,
          deliver,
        } => {
          if self.running_reads.get() >= MAX_RUNNING_READS {
            postponed.push_back(StreamRequest::Read {
              token,
              start,
              deliver,
            });
            continue;
          }
          if let Some(started) = self.start_read(client, token, start, deliver).await {
            work.push(started);
          }
        },
      }
    }
    self.waiting = postponed;
    work
  }

  async fn start_open<A: ServerApi, S: LocalStore>(
    &mut self,
    client: &Client<A, S>,
    reference: AttachmentRef,
    deliver: DeliverOpened,
  ) -> Option<StreamWork> {
    let Some(media_type) = streamable_type(&reference.media_type) else {
      deliver(Err("this file doesn't stream".to_owned()));
      return None;
    };
    let Some(key) = attachment_key(&reference) else {
      deliver(Err("bad attachment key".to_owned()));
      return None;
    };
    let location = match fresh_location(client, &reference).await {
      Ok(location) => location,
      Err(error) => {
        deliver(Err(error));
        return None;
      },
    };
    let header = client.blob_range(&location.url, 0, HEADER_LEN as u64);
    let token = MediaStreamToken::generate();
    let state = Rc::new(RefCell::new(StreamState {
      reference,
      media_type,
      maybe_location: Some(location.clone()),
      maybe_decryptor: None,
    }));
    if self.open.len() >= MAX_OPEN_STREAMS {
      self.open.remove(0);
    }
    self.open.push(OpenStream {
      token: token.clone(),
      state: state.clone(),
    });
    Some(Box::pin(async move {
      let opened = header
        .await
        .map_err(|error| error.to_string())
        .and_then(|header| {
          AttachmentRangeDecryptor::new(&key, &header, location.ciphertext_len)
            .map_err(|error| error.to_string())
        });
      match opened {
        Ok(decryptor) => {
          let size_bytes = decryptor.layout().plaintext_len();
          state.borrow_mut().maybe_decryptor = Some(Rc::new(decryptor));
          deliver(Ok(OpenedStream {
            token,
            size_bytes,
            media_type,
          }));
        },
        Err(error) => deliver(Err(error)),
      }
    }))
  }

  async fn start_read<A: ServerApi, S: LocalStore>(
    &mut self,
    client: &Client<A, S>,
    token: MediaStreamToken,
    start: u64,
    deliver: DeliverPart,
  ) -> Option<StreamWork> {
    let Some(index) = self.open.iter().position(|stream| stream.token == token) else {
      deliver(Err("the stream is closed".to_owned()));
      return None;
    };
    // Most recently used last.
    let stream = self.open.remove(index);
    let state = stream.state.clone();
    self.open.push(stream);
    let Some(decryptor) = state.borrow().maybe_decryptor.clone() else {
      deliver(Err("the stream is not open yet".to_owned()));
      return None;
    };
    let total_bytes = decryptor.layout().plaintext_len();
    let media_type = state.borrow().media_type;
    let Some(span) = decryptor
      .layout()
      .span(start, start.saturating_add(MAX_READ_BYTES))
    else {
      deliver(Ok(StreamPart {
        start,
        bytes: SecretBytes::new(Vec::new()),
        total_bytes,
        media_type,
      }));
      return None;
    };
    let ciphertext_len = decryptor.layout().ciphertext_len();
    let location = match current_location(client, &state, ciphertext_len).await {
      Ok(location) => location,
      Err(error) => {
        deliver(Err(error));
        return None;
      },
    };
    let request = client.blob_range(&location.url, span.ciphertext_start, span.ciphertext_end);
    let running = self.running_reads.clone();
    running.set(running.get() + 1);
    Some(Box::pin(async move {
      let read = request.await.map_err(|error| error.to_string());
      running.set(running.get().saturating_sub(1));
      let part = match read {
        Ok(ciphertext) => decryptor
          .decrypt_span(&span, &ciphertext)
          .map_err(|error| error.to_string())
          .map(|plaintext| {
            let end = start.saturating_add(MAX_READ_BYTES).min(total_bytes);
            let from = (start - span.plaintext_start) as usize;
            let to = (end - span.plaintext_start) as usize;
            StreamPart {
              start,
              bytes: SecretBytes::new(plaintext.expose_secret()[from..to].to_vec()),
              total_bytes,
              media_type,
            }
          }),
        Err(error) => {
          // An expired URL (or a clock that ran differently) fails here: the next read asks again.
          state.borrow_mut().maybe_location = None;
          Err(error)
        },
      };
      deliver(part);
    }))
  }
}

impl MediaStreamToken {
  fn generate() -> Self {
    let mut bytes = [0_u8; TOKEN_BYTES];
    OsRng.fill_bytes(&mut bytes);
    Self(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
  }

  /// A token from a URL; `None` unless it has the shape of one.
  pub fn parse(raw: &str) -> Option<Self> {
    let is_token = raw.len() == TOKEN_BYTES * 2
      && raw
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    is_token.then(|| Self(raw.to_owned()))
  }

  pub fn as_str(&self) -> &str {
    &self.0
  }
}

impl fmt::Debug for MediaStreamToken {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    // It opens the file for whoever has it: never in a log.
    f.write_str("MediaStreamToken([redacted])")
  }
}

impl fmt::Debug for StreamPart {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("StreamPart")
      .field("start", &self.start)
      .field("len", &self.bytes.len())
      .field("total_bytes", &self.total_bytes)
      .finish()
  }
}

fn attachment_key(reference: &AttachmentRef) -> Option<AttachmentKey> {
  SymmetricKey::from_slice(reference.key.as_slice()).map(AttachmentKey::from_key)
}

/// The stream's download URL, asked for again when it is old or a read failed with it.
async fn current_location<A: ServerApi, S: LocalStore>(
  client: &Client<A, S>,
  state: &Rc<RefCell<StreamState>>,
  ciphertext_len: u64,
) -> Result<Location, String> {
  let maybe_location = state.borrow().maybe_location.clone();
  if let Some(location) = maybe_location
    && client.now_ms().saturating_sub(location.fetched_ms) < URL_REUSE_MS
  {
    return Ok(location);
  }
  let reference = state.borrow().reference.clone();
  let fresh = fresh_location(client, &reference).await?;
  // The decryptor's layout is the length the stream opened with; a blob that changed size since
  // is not the same blob.
  if fresh.ciphertext_len != ciphertext_len {
    return Err("the file changed while it played".to_owned());
  }
  state.borrow_mut().maybe_location = Some(fresh.clone());
  Ok(fresh)
}

async fn fresh_location<A: ServerApi, S: LocalStore>(
  client: &Client<A, S>,
  reference: &AttachmentRef,
) -> Result<Location, String> {
  let (url, ciphertext_len) = client
    .blob_location(reference.attachment_id)
    .await
    .map_err(|error| error.to_string())?;
  Ok(Location {
    url,
    ciphertext_len,
    fetched_ms: client.now_ms(),
  })
}
