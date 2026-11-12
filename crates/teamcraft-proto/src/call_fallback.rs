//! Framing of a call on the SFU's WebSocket fallback, for networks that block UDP (ARCHITECTURE.md
//! §6.6).
//!
//! One frame is one binary WebSocket message: a kind byte, then what QUIC would carry there. The
//! WebSocket keeps message boundaries, so a signaling frame holds one bare JSON message, without
//! the length prefix of the QUIC signaling stream, and a datagram frame one media datagram as it
//! is on QUIC, DAVE-encrypted payload included. The first frame of a session is the same
//! [`crate::CallClientMessage::Join`] with the same join token as on QUIC.
//!
//! A close frame carries a close code as QUIC does ([`crate::SFU_GOING_AWAY_CLOSE_CODE`]); the
//! WebSocket's own codes have no room for them, and a reverse proxy may not pass them on anyway.
//!
//! ```text
//! 0x01 | JSON message                 signaling
//! 0x02 | media datagram               media
//! 0x03 | close code (u32 BE) | reason the sender closes on purpose
//! ```
//!
//! The join token also goes along with the upgrade, in `Sec-WebSocket-Protocol` (the one header a
//! browser lets a page set): the client offers [`CALL_FALLBACK_PROTOCOL`] and
//! [`join_token_protocol`], and the SFU checks the token before the upgrade and answers with
//! [`CALL_FALLBACK_PROTOCOL`] alone, so the token never comes back. A connection that can't show a
//! token holds no more than an anonymous one may (#1297). The `join` still carries the same token:
//! SFUs and clients of before don't know the header.
//!
//! Frames come from untrusted peers: [`FallbackFrame::decode`] never panics, and refuses a datagram
//! bigger than any QUIC could carry. A kind it doesn't know is [`FallbackFrame::Unknown`], for
//! receivers to ignore: newer peers may add some.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use thiserror::Error;

/// HTTP path of the WebSocket on the SFU's fallback listener.
pub const CALL_FALLBACK_PATH: &str = "/call/ws";

/// The subprotocol the SFU answers with when the client offered it.
pub const CALL_FALLBACK_PROTOCOL: &str = "teamcraft-call";

/// In front of the join token in the subprotocol that carries it.
pub const JOIN_TOKEN_PROTOCOL_PREFIX: &str = "teamcraft-join.";

/// The subprotocols a client offers: [`CALL_FALLBACK_PROTOCOL`] first, then its join token.
pub fn fallback_protocols(token: &str) -> [String; 2] {
  [CALL_FALLBACK_PROTOCOL.to_owned(), join_token_protocol(token)]
}

/// The join token as a subprotocol: base64url, as a subprotocol may hold only an HTTP token's
/// characters (a development token has colons).
pub fn join_token_protocol(token: &str) -> String {
  format!(
    "{JOIN_TOKEN_PROTOCOL_PREFIX}{}",
    URL_SAFE_NO_PAD.encode(token.as_bytes())
  )
}

/// What the client offered in its `Sec-WebSocket-Protocol` lines: whether it knows
/// [`CALL_FALLBACK_PROTOCOL`], and its join token if it sent one. `Err` for a token that isn't
/// base64url of UTF-8, or more than one token.
pub fn parse_offered_protocols<'a>(
  lines: impl IntoIterator<Item = &'a str>,
) -> Result<OfferedProtocols, OfferedProtocolsError> {
  let mut offered = OfferedProtocols::default();
  for protocol in lines.into_iter().flat_map(|line| line.split(',')).map(str::trim) {
    if protocol == CALL_FALLBACK_PROTOCOL {
      offered.knows_call_protocol = true;
    } else if let Some(encoded) = protocol.strip_prefix(JOIN_TOKEN_PROTOCOL_PREFIX) {
      if offered.maybe_token.is_some() {
        return Err(OfferedProtocolsError::SeveralTokens);
      }
      let bytes = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| OfferedProtocolsError::Malformed)?;
      let token = String::from_utf8(bytes).map_err(|_| OfferedProtocolsError::Malformed)?;
      offered.maybe_token = Some(token);
    }
  }
  Ok(offered)
}

/// The subprotocols of a fallback upgrade, as far as the SFU cares.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OfferedProtocols {
  pub knows_call_protocol: bool,
  pub maybe_token: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum OfferedProtocolsError {
  #[error("the join token subprotocol is not base64url of UTF-8")]
  Malformed,
  #[error("more than one join token subprotocol")]
  SeveralTokens,
}

/// Largest media datagram on the fallback. Clients send at most 1200 bytes, and the SFU forwards
/// them with at most 2 bytes more; nothing bigger fits a QUIC datagram either.
pub const MAX_FALLBACK_DATAGRAM_BYTES: usize = 1500;

/// Longest reason of a close frame (as a QUIC close, a few words).
pub const MAX_FALLBACK_CLOSE_REASON_BYTES: usize = 128;

const KIND_SIGNAL: u8 = 0x01;
const KIND_DATAGRAM: u8 = 0x02;
const KIND_CLOSE: u8 = 0x03;
const CLOSE_CODE_BYTES: usize = 4;

/// One frame, borrowing its payload from the received message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackFrame<'a> {
  /// One JSON signaling message.
  Signal(&'a [u8]),
  /// One media datagram.
  Datagram(&'a [u8]),
  /// The sender closes the session on purpose.
  Close { code: u32, reason: &'a [u8] },
  /// A kind of a newer peer.
  Unknown(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum FallbackFrameError {
  #[error("empty frame")]
  Empty,
  #[error("a close frame needs its {CLOSE_CODE_BYTES}-byte code, it has {actual_bytes} bytes")]
  TruncatedClose { actual_bytes: usize },
  #[error("a close reason of {actual_bytes} bytes exceeds {MAX_FALLBACK_CLOSE_REASON_BYTES}")]
  CloseReasonTooLong { actual_bytes: usize },
  #[error("a datagram of {actual_bytes} bytes exceeds {MAX_FALLBACK_DATAGRAM_BYTES}")]
  DatagramTooLarge { actual_bytes: usize },
}

impl<'a> FallbackFrame<'a> {
  /// Reads one received message.
  pub fn decode(message: &'a [u8]) -> Result<Self, FallbackFrameError> {
    let (&kind, body) = message.split_first().ok_or(FallbackFrameError::Empty)?;
    match kind {
      KIND_SIGNAL => Ok(Self::Signal(body)),
      KIND_DATAGRAM if body.len() > MAX_FALLBACK_DATAGRAM_BYTES => {
        Err(FallbackFrameError::DatagramTooLarge {
          actual_bytes: body.len(),
        })
      },
      KIND_DATAGRAM => Ok(Self::Datagram(body)),
      KIND_CLOSE => decode_close(body),
      other => Ok(Self::Unknown(other)),
    }
  }

  /// The message to send for this frame. A close reason is cut to its limit.
  pub fn encode(&self) -> Vec<u8> {
    match self {
      Self::Signal(body) => tagged(KIND_SIGNAL, body),
      Self::Datagram(body) => tagged(KIND_DATAGRAM, body),
      Self::Close { code, reason } => {
        let reason = &reason[..reason.len().min(MAX_FALLBACK_CLOSE_REASON_BYTES)];
        let mut frame = Vec::with_capacity(1 + CLOSE_CODE_BYTES + reason.len());
        frame.push(KIND_CLOSE);
        frame.extend_from_slice(&code.to_be_bytes());
        frame.extend_from_slice(reason);
        frame
      },
      Self::Unknown(kind) => vec![*kind],
    }
  }
}

fn decode_close(body: &[u8]) -> Result<FallbackFrame<'_>, FallbackFrameError> {
  let Some((code, reason)) = body.split_first_chunk::<CLOSE_CODE_BYTES>() else {
    return Err(FallbackFrameError::TruncatedClose {
      actual_bytes: body.len(),
    });
  };
  if reason.len() > MAX_FALLBACK_CLOSE_REASON_BYTES {
    return Err(FallbackFrameError::CloseReasonTooLong {
      actual_bytes: reason.len(),
    });
  }
  Ok(FallbackFrame::Close {
    code: u32::from_be_bytes(*code),
    reason,
  })
}

fn tagged(kind: u8, body: &[u8]) -> Vec<u8> {
  let mut frame = Vec::with_capacity(1 + body.len());
  frame.push(kind);
  frame.extend_from_slice(body);
  frame
}

#[cfg(test)]
mod tests {
  use super::*;

  mod wire_format {
    use super::*;

    #[test]
    fn frames_have_these_bytes() {
      assert_eq!(FallbackFrame::Signal(b"{}").encode(), b"\x01{}");
      assert_eq!(FallbackFrame::Datagram(b"\xAA\xBB").encode(), b"\x02\xAA\xBB");
      assert_eq!(
        FallbackFrame::Close {
          code: 0x4805,
          reason: b"going away"
        }
        .encode(),
        b"\x03\x00\x00\x48\x05going away"
      );
    }

    #[test]
    fn frames_survive_a_round_trip() {
      let frames = [
        FallbackFrame::Signal(b"{\"type\":\"ping\"}"),
        FallbackFrame::Datagram(&[0; MAX_FALLBACK_DATAGRAM_BYTES]),
        FallbackFrame::Datagram(b""),
        FallbackFrame::Close { code: 0, reason: b"" },
        FallbackFrame::Close {
          code: u32::MAX,
          reason: &[b'x'; MAX_FALLBACK_CLOSE_REASON_BYTES],
        },
      ];
      for frame in frames {
        assert_eq!(FallbackFrame::decode(&frame.encode()), Ok(frame));
      }
    }

    #[test]
    fn a_kind_of_a_newer_peer_is_no_error() {
      assert_eq!(
        FallbackFrame::decode(b"\x7Fwhatever"),
        Ok(FallbackFrame::Unknown(0x7F))
      );
    }
  }

  mod hostile_input {
    use super::*;

    #[test]
    fn an_empty_message_is_refused() {
      assert_eq!(FallbackFrame::decode(b""), Err(FallbackFrameError::Empty));
    }

    #[test]
    fn a_truncated_close_is_refused() {
      for length in 0..CLOSE_CODE_BYTES {
        let mut message = vec![KIND_CLOSE];
        message.resize(1 + length, 0);
        assert_eq!(
          FallbackFrame::decode(&message),
          Err(FallbackFrameError::TruncatedClose { actual_bytes: length })
        );
      }
    }

    #[test]
    fn an_oversized_datagram_is_refused() {
      let mut message = vec![KIND_DATAGRAM];
      message.resize(2 + MAX_FALLBACK_DATAGRAM_BYTES, 0xFF);
      assert_eq!(
        FallbackFrame::decode(&message),
        Err(FallbackFrameError::DatagramTooLarge {
          actual_bytes: MAX_FALLBACK_DATAGRAM_BYTES + 1
        })
      );
    }

    #[test]
    fn an_oversized_close_reason_is_refused() {
      let mut message = vec![KIND_CLOSE, 0, 0, 0, 0];
      message.resize(5 + MAX_FALLBACK_CLOSE_REASON_BYTES + 1, b'x');
      assert_eq!(
        FallbackFrame::decode(&message),
        Err(FallbackFrameError::CloseReasonTooLong {
          actual_bytes: MAX_FALLBACK_CLOSE_REASON_BYTES + 1
        })
      );
    }

    #[test]
    fn encoding_cuts_a_long_close_reason_so_it_decodes() {
      let reason = [b'x'; MAX_FALLBACK_CLOSE_REASON_BYTES * 2];
      let encoded = FallbackFrame::Close {
        code: 1,
        reason: &reason,
      }
      .encode();
      assert!(FallbackFrame::decode(&encoded).is_ok());
    }

    #[test]
    fn a_signal_is_not_read_here_so_garbage_passes_to_the_json_parser() {
      assert_eq!(
        FallbackFrame::decode(b"\x01\xFF\xFE"),
        Ok(FallbackFrame::Signal(b"\xFF\xFE"))
      );
    }
  }

  mod protocols {
    use super::*;

    #[test]
    fn a_token_round_trips_through_its_subprotocol() {
      let token = "dev:1:2:3";
      let [call, carried] = fallback_protocols(token);
      assert_eq!(call, CALL_FALLBACK_PROTOCOL);
      assert!(
        carried
          .bytes()
          .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
      );
      let line = format!("{call}, {carried}");
      let offered = parse_offered_protocols([line.as_str()]).unwrap();
      assert!(offered.knows_call_protocol);
      assert_eq!(offered.maybe_token.as_deref(), Some(token));
    }

    #[test]
    fn several_lines_and_other_protocols_are_read_too() {
      let carried = join_token_protocol("t");
      let offered = parse_offered_protocols(["chat", carried.as_str()]).unwrap();
      assert!(!offered.knows_call_protocol);
      assert_eq!(offered.maybe_token.as_deref(), Some("t"));
      assert_eq!(
        parse_offered_protocols(std::iter::empty()),
        Ok(OfferedProtocols::default())
      );
    }

    #[test]
    fn a_broken_or_second_token_is_refused() {
      assert_eq!(
        parse_offered_protocols(["teamcraft-join.!!"]),
        Err(OfferedProtocolsError::Malformed)
      );
      let carried = join_token_protocol("t");
      assert_eq!(
        parse_offered_protocols([carried.as_str(), carried.as_str()]),
        Err(OfferedProtocolsError::SeveralTokens)
      );
    }
  }
}
