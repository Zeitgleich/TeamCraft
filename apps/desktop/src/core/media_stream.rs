//! Videos that play while they stream (#200, `teamcraft_core::media_stream`): the webview's player
//! loads `http://127.0.0.1:<port>/<token>` and reads it in ranges as from a web server; each range is
//! read by the core from the encrypted blob and decrypted, so a video of any size plays and seeks
//! without the whole file in memory.
//!
//! Why a loopback server and not a custom URI scheme: WebKitGTK plays no media from a custom scheme
//! (not even a whole `200` answer), and it doesn't ask again after a short `206`. So every answer
//! covers the whole range asked for, streamed: the body pulls one read of the core after the other
//! while the player takes them, and stops when the player closes the connection (a seek, or the
//! player gone). Memory stays at a read or two per connection.
//!
//! Only this app's webview has a reason to come here, but any process of the machine could: a
//! request needs the random token of a stream the page opened (128 bits), the server's own
//! `Host` (a web page that rebinds a name to 127.0.0.1 sends its own), and gets nothing else. The
//! server binds the loopback address only and starts with the first stream.

use std::convert::Infallible;
use std::future::Future;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Empty, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper::header::{
  ACCEPT_RANGES, CACHE_CONTROL, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, HOST, HeaderValue, RANGE,
  X_CONTENT_TYPE_OPTIONS,
};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use serde::Serialize;
use tauri::{AppHandle, Manager, State};
use teamcraft_core::media_stream::{MediaStreamToken, StreamPart};
use teamcraft_proto::{ConversationId, MessageId};
use tokio::net::TcpListener;
use tokio::sync::{OnceCell, Semaphore};

use super::commands::{CommandError, CoreHost};
use super::runtime::CoreRuntime;

/// Connections served at once: a player opens one or two per video.
const MAX_CONNECTIONS: usize = 16;
/// A request's head must arrive within this; a connection that sends nothing is closed.
const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// The loopback server of the app, started with the first stream.
#[derive(Default)]
pub struct MediaServer {
  maybe_address: OnceCell<SocketAddr>,
}

/// A stream that opened, for the page's player.
#[derive(Serialize)]
pub struct StreamHandle {
  /// What the player loads.
  url: String,
  /// The token, to close the stream again.
  token: String,
  size_bytes: u64,
  media_type: &'static str,
}

/// What an answer to a range is, from the core's first read.
#[derive(Debug, PartialEq, Eq)]
enum Plan {
  /// The range starts at or past the end of the file.
  NotSatisfiable { total_bytes: u64 },
  /// Bytes `start..=last` of `total_bytes`.
  Partial { start: u64, last: u64, total_bytes: u64 },
}

/// Opens a video of a message to stream; answers the URL its player loads.
#[tauri::command(rename_all = "snake_case")]
pub async fn core_open_media_stream(
  app: AppHandle,
  host: State<'_, CoreHost>,
  server: State<'_, MediaServer>,
  conversation: ConversationId,
  message: MessageId,
  file_id: String,
) -> Result<StreamHandle, CommandError> {
  let address = server.address(&app).await?;
  let opened = host
    .current()?
    .open_stream(conversation, message, file_id)
    .await?;
  Ok(StreamHandle {
    url: format!("http://{address}/{}", opened.token.as_str()),
    token: opened.token.as_str().to_owned(),
    size_bytes: opened.size_bytes,
    media_type: opened.media_type,
  })
}

/// Closes a stream the page no longer plays.
#[tauri::command(rename_all = "snake_case")]
pub fn core_close_media_stream(host: State<'_, CoreHost>, token: String) -> Result<(), CommandError> {
  if let Some(token) = MediaStreamToken::parse(&token) {
    host.current()?.close_stream(token);
  }
  Ok(())
}

impl MediaServer {
  /// The server's address; binds it the first time.
  async fn address(&self, app: &AppHandle) -> Result<SocketAddr, String> {
    self
      .maybe_address
      .get_or_try_init(|| async {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
          .await
          .map_err(|error| format!("could not listen for streamed videos: {error}"))?;
        let address = listener
          .local_addr()
          .map_err(|error| format!("could not listen for streamed videos: {error}"))?;
        tauri::async_runtime::spawn(serve(listener, app.clone(), address));
        tracing::info!("serving streamed videos on the loopback address");
        Ok(address)
      })
      .await
      .copied()
  }
}

async fn serve(listener: TcpListener, app: AppHandle, address: SocketAddr) {
  let connections = Arc::new(Semaphore::new(MAX_CONNECTIONS));
  loop {
    let (stream, _) = match listener.accept().await {
      Ok(accepted) => accepted,
      Err(error) => {
        // Out of file descriptors, say: wait a moment instead of spinning.
        tracing::warn!(%error, "could not accept a streamed video's connection");
        tokio::time::sleep(Duration::from_millis(100)).await;
        continue;
      },
    };
    let Ok(permit) = connections.clone().try_acquire_owned() else {
      tracing::debug!("too many streamed video connections; one is refused");
      continue;
    };
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
      let service = service_fn(move |request| {
        let app = app.clone();
        async move { Ok::<_, Infallible>(answer(&app, address, &request).await) }
      });
      let served = http1::Builder::new()
        .timer(TokioTimer::new())
        .header_read_timeout(HEADER_READ_TIMEOUT)
        .serve_connection(TokioIo::new(stream), service)
        .await;
      if let Err(error) = served {
        // A player that seeks closes the connection mid-answer: not worth more than a debug line.
        tracing::debug!(%error, "a streamed video's connection ended");
      }
      drop(permit);
    });
  }
}

/// Answers one request of the player: a range of the stream its path names.
async fn answer(
  app: &AppHandle,
  address: SocketAddr,
  request: &Request<Incoming>,
) -> Response<BoxBody<Bytes, Infallible>> {
  if request.method() != Method::GET && request.method() != Method::HEAD {
    return status_only(StatusCode::METHOD_NOT_ALLOWED);
  }
  let is_own_host = request
    .headers()
    .get(HOST)
    .and_then(|host| host.to_str().ok())
    .is_some_and(|host| host == address.to_string());
  let maybe_token = MediaStreamToken::parse(request.uri().path().trim_start_matches('/'));
  let Some(token) = maybe_token.filter(|_| is_own_host) else {
    return status_only(StatusCode::NOT_FOUND);
  };
  let Some((start, maybe_end)) =
    requested_range(request.headers().get(RANGE).and_then(|value| value.to_str().ok()))
  else {
    return status_only(StatusCode::RANGE_NOT_SATISFIABLE);
  };
  let runtime = match app.state::<CoreHost>().current() {
    Ok(runtime) => runtime,
    Err(error) => {
      tracing::debug!(%error, "a streamed video was asked for without a core");
      return status_only(StatusCode::SERVICE_UNAVAILABLE);
    },
  };
  let first = match runtime.read_stream(token.clone(), start).await {
    Ok(first) => first,
    Err(error) => {
      tracing::warn!(%error, "could not read a streamed video");
      return status_only(StatusCode::BAD_GATEWAY);
    },
  };
  let media_type = first.media_type;
  match plan(&first, maybe_end) {
    Plan::NotSatisfiable { total_bytes } => {
      let mut response = status_only(StatusCode::RANGE_NOT_SATISFIABLE);
      if let Ok(value) = HeaderValue::from_str(&format!("bytes */{total_bytes}")) {
        response.headers_mut().insert(CONTENT_RANGE, value);
      }
      response
    },
    Plan::Partial {
      start,
      last,
      total_bytes,
    } => {
      let body = match request.method() == Method::HEAD {
        true => Empty::new().boxed(),
        false => {
          let pieces = pieces(first, last, move |at| {
            let (runtime, token) = (runtime.clone(), token.clone());
            async move { read_with(&runtime, token, at).await }
          });
          StreamBody::new(pieces).boxed()
        },
      };
      Response::builder()
        .status(StatusCode::PARTIAL_CONTENT)
        .header(CONTENT_TYPE, media_type)
        .header(CONTENT_LENGTH, last - start + 1)
        .header(CONTENT_RANGE, format!("bytes {start}-{last}/{total_bytes}"))
        .header(ACCEPT_RANGES, "bytes")
        .header(CACHE_CONTROL, "no-store")
        .header(X_CONTENT_TYPE_OPTIONS, "nosniff")
        .body(body)
        .unwrap_or_else(|_| status_only(StatusCode::INTERNAL_SERVER_ERROR))
    },
  }
}

async fn read_with(
  runtime: &Arc<CoreRuntime>,
  token: MediaStreamToken,
  at: u64,
) -> Result<StreamPart, String> {
  runtime.read_stream(token, at).await
}

/// The first byte asked for and the last (included), from `Range: bytes=<start>-[<end>]`; no header
/// is the whole file from the start. `None` for what a player doesn't send (several ranges, a range
/// from the end).
fn requested_range(maybe_header: Option<&str>) -> Option<(u64, Option<u64>)> {
  let Some(header) = maybe_header else {
    return Some((0, None));
  };
  let (start, end) = header.trim().strip_prefix("bytes=")?.split_once('-')?;
  let start = start.trim().parse::<u64>().ok()?;
  let maybe_end = match end.trim() {
    "" => None,
    end => Some(end.parse::<u64>().ok().filter(|end| *end >= start)?),
  };
  Some((start, maybe_end))
}

/// The answer to a range whose first read came back as `first`: up to the end asked for, or the
/// end of the file.
fn plan(first: &StreamPart, maybe_end: Option<u64>) -> Plan {
  if first.bytes.is_empty() || first.start >= first.total_bytes {
    return Plan::NotSatisfiable {
      total_bytes: first.total_bytes,
    };
  }
  let end_of_file = first.total_bytes - 1;
  Plan::Partial {
    start: first.start,
    last: maybe_end.map_or(end_of_file, |end| end.min(end_of_file)),
    total_bytes: first.total_bytes,
  }
}

/// The body of an answer: `first`, then one read after the other up to `last`, each pulled only
/// when the connection took the one before. A read that fails ends the body early, which the
/// player sees as a broken connection and asks again.
fn pieces<R, F>(
  first: StreamPart,
  last: u64,
  read: R,
) -> impl futures_util::Stream<Item = Result<Frame<Bytes>, Infallible>> + Send + 'static
where
  R: Fn(u64) -> F + Send + 'static,
  F: Future<Output = Result<StreamPart, String>> + Send + 'static,
{
  let state = Pieces {
    at: first.start,
    maybe_first: Some(first),
    last,
    read,
  };
  futures_util::stream::unfold(state, |mut state| async move {
    if state.at > state.last {
      return None;
    }
    let part = match state.maybe_first.take() {
      Some(part) => part,
      None => match (state.read)(state.at).await {
        Ok(part) => part,
        Err(error) => {
          tracing::warn!(%error, "a streamed video's read failed midway");
          return None;
        },
      },
    };
    if part.bytes.is_empty() || part.start != state.at {
      return None;
    }
    // Invariant: a read is at most a few MiB, far below `usize::MAX`.
    let wanted = (state.last - state.at + 1).min(part.bytes.len() as u64) as usize;
    let bytes = Bytes::copy_from_slice(&part.bytes.expose_secret()[..wanted]);
    state.at += wanted as u64;
    Some((Ok(Frame::data(bytes)), state))
  })
}

/// Where [`pieces`] is: the next byte to send, the last one, and how to read on.
struct Pieces<R> {
  at: u64,
  maybe_first: Option<StreamPart>,
  last: u64,
  read: R,
}

fn status_only(status: StatusCode) -> Response<BoxBody<Bytes, Infallible>> {
  let mut response = Response::new(Empty::new().boxed());
  *response.status_mut() = status;
  response
}

#[cfg(test)]
mod tests {
  use std::sync::Mutex;

  use futures_util::StreamExt;
  use teamcraft_crypto::secret::SecretBytes;

  use super::*;

  const TOTAL: u64 = 10_000;
  const READ: u64 = 3_000;

  #[test]
  fn ranges_players_send_are_read() {
    assert_eq!(requested_range(None), Some((0, None)));
    assert_eq!(requested_range(Some("bytes=0-")), Some((0, None)));
    assert_eq!(requested_range(Some("bytes=100-199")), Some((100, Some(199))));
    assert_eq!(requested_range(Some(" bytes=7-7 ")), Some((7, Some(7))));
  }

  #[test]
  fn other_ranges_are_refused() {
    for header in [
      "bytes=-500",
      "bytes=5-2",
      "bytes=0-1,5-6",
      "items=0-",
      "bytes=a-",
      "",
    ] {
      assert_eq!(requested_range(Some(header)), None, "{header}");
    }
  }

  #[test]
  fn an_answer_covers_the_range_asked_for_or_the_rest_of_the_file() {
    let first = part(1000);
    assert_eq!(
      plan(&first, None),
      Plan::Partial {
        start: 1000,
        last: TOTAL - 1,
        total_bytes: TOTAL
      }
    );
    assert_eq!(
      plan(&first, Some(1009)),
      Plan::Partial {
        start: 1000,
        last: 1009,
        total_bytes: TOTAL
      }
    );
    assert_eq!(
      plan(&first, Some(TOTAL + 50)),
      Plan::Partial {
        start: 1000,
        last: TOTAL - 1,
        total_bytes: TOTAL
      }
    );
    assert_eq!(
      plan(&part(TOTAL), None),
      Plan::NotSatisfiable { total_bytes: TOTAL }
    );
  }

  #[tokio::test]
  async fn the_body_reads_on_until_the_end_of_the_range() {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let log = asked.clone();
    let body: Vec<Bytes> = pieces(part(500), TOTAL - 1, move |at| {
      log.lock().unwrap().push(at);
      std::future::ready(Ok(part(at)))
    })
    .map(|frame| frame.unwrap().into_data().unwrap())
    .collect()
    .await;
    let sent: Vec<u8> = body.concat();
    assert_eq!(sent.len() as u64, TOTAL - 500);
    assert_eq!(sent, (500..TOTAL).map(byte_at).collect::<Vec<_>>());
    assert_eq!(*asked.lock().unwrap(), vec![3500, 6500, 9500]);
  }

  #[tokio::test]
  async fn the_body_stops_at_the_asked_end_and_on_a_failed_read() {
    let cut: Vec<Bytes> = pieces(part(0), 99, |at| std::future::ready(Ok(part(at))))
      .map(|frame| frame.unwrap().into_data().unwrap())
      .collect()
      .await;
    assert_eq!(cut.concat().len(), 100);
    let broken: Vec<Bytes> = pieces(part(0), TOTAL - 1, |_| {
      std::future::ready(Err("offline".to_owned()))
    })
    .map(|frame| frame.unwrap().into_data().unwrap())
    .collect()
    .await;
    assert_eq!(broken.concat().len() as u64, READ, "only the first read");
  }

  /// What the core reads from `start`: up to [`READ`] bytes of a file of [`TOTAL`].
  fn part(start: u64) -> StreamPart {
    let end = (start + READ).min(TOTAL);
    StreamPart {
      start,
      bytes: SecretBytes::new((start..end.max(start)).map(byte_at).collect()),
      total_bytes: TOTAL,
      media_type: "video/mp4",
    }
  }

  fn byte_at(position: u64) -> u8 {
    (position % 251) as u8
  }
}
