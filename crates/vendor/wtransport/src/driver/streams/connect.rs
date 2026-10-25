use std::borrow::Cow;
use tracing::debug;
use wtransport_proto::frame::Frame;
use wtransport_proto::bytes::IoReadError;
use wtransport_proto::capsule::capsules;
use wtransport_proto::capsule::Capsule;
use wtransport_proto::capsule::{self};
use wtransport_proto::error::ErrorCode;
use wtransport_proto::frame::FrameKind;
use wtransport_proto::varint::VarInt;

use super::session::StreamSession;
use super::ProtoReadError;
use crate::driver::DriverError;
use crate::error::ApplicationClose;
use std::future::pending;

pub struct ConnectStream {
    stream: Option<StreamSession>,
}

impl ConnectStream {
    pub fn empty() -> Self {
        Self { stream: None }
    }

    pub fn is_empty(&self) -> bool {
        self.stream.is_none()
    }

    pub fn set_stream(&mut self, stream: StreamSession) {
        self.stream = Some(stream);
    }

    /// Sends a CLOSE_WEBTRANSPORT_SESSION capsule with `error_code` and `reason`, ends the CONNECT
    /// stream and waits until the peer has the data. A no-op without a session stream.
    pub async fn send_close(&mut self, error_code: u32, reason: &str) {
        let Some(stream) = self.stream.as_mut() else {
            return;
        };

        let payload = close_capsule(error_code, reason);

        if stream
            .write_frame(Frame::new_data(Cow::Owned(payload)))
            .await
            .is_err()
        {
            return;
        }

        stream.finish().await;
    }

    pub async fn run(&mut self) -> DriverError {
        let stream = match self.stream.as_mut() {
            Some(stream) => stream,
            None => pending().await,
        };

        loop {
            return match stream.read_frame().await {
                Ok(frame) => {
                    if !matches!(frame.kind(), FrameKind::Data) {
                        debug!("Skipping non-data frame of kind {:?}", frame.kind());
                        continue;
                    }

                    let capsule = match Capsule::with_frame(&frame) {
                        Some(capsule)
                            if matches!(
                                capsule.kind(),
                                capsule::CapsuleKind::CloseWebTransportSession
                            ) =>
                        {
                            capsule
                        }
                        // Unknown capsule, skip it
                        _ => {
                            debug!(
                                "Unknown capsule of kind {:?} of {}B",
                                frame.kind(),
                                frame.payload().len()
                            );

                            continue;
                        }
                    };

                    let close_session =
                        match capsules::CloseWebTransportSession::with_capsule(&capsule) {
                            Ok(close_session) => close_session,
                            Err(error_code) => return DriverError::Proto(error_code),
                        };

                    // reset right away to avoid receiving additional data which requires resetting with ErrorCode::Message.
                    self.stream
                        .take()
                        .unwrap()
                        .reset(ErrorCode::NoError.to_code());

                    DriverError::ApplicationClosed(ApplicationClose::new(
                        close_session.error_code(),
                        close_session
                            .reason()
                            .as_bytes()
                            .to_vec()
                            .into_boxed_slice(),
                    ))
                }
                Err(ProtoReadError::H3(error_code)) => DriverError::Proto(error_code),
                Err(ProtoReadError::IO(io_error)) => match io_error {
                    // Cleanly terminating a CONNECT stream without a CLOSE_WEBTRANSPORT_SESSION
                    // capsule SHALL be semantically equivalent to terminating it with a
                    // CLOSE_WEBTRANSPORT_SESSION capsule that has an error code of 0 and an empty error string.
                    IoReadError::ImmediateFin => DriverError::ApplicationClosed(
                        ApplicationClose::new(VarInt::from_u32(0), Box::new([])),
                    ),
                    IoReadError::UnexpectedFin | IoReadError::Reset => {
                        DriverError::Proto(ErrorCode::ClosedCriticalStream)
                    }
                    IoReadError::NotConnected => DriverError::NotConnected,
                },
            };
        }
    }
}

/// Longest reason a CLOSE_WEBTRANSPORT_SESSION capsule carries (draft-ietf-webtrans-http3).
const MAX_CLOSE_REASON_LEN: usize = 1024;

/// A CLOSE_WEBTRANSPORT_SESSION capsule (type 0x2843): the 32-bit application error code, then
/// the reason, cut to the maximum at a character boundary.
fn close_capsule(error_code: u32, reason: &str) -> Vec<u8> {
    let mut end = reason.len().min(MAX_CLOSE_REASON_LEN);
    while !reason.is_char_boundary(end) {
        end -= 1;
    }
    let reason = &reason.as_bytes()[..end];

    let mut capsule = Vec::with_capacity(8 + reason.len());
    put_varint(&mut capsule, 0x2843);
    put_varint(&mut capsule, 4 + reason.len() as u64);
    capsule.extend_from_slice(&error_code.to_be_bytes());
    capsule.extend_from_slice(reason);
    capsule
}

/// Appends `value` (below 2^14) as a QUIC variable-length integer.
fn put_varint(buffer: &mut Vec<u8>, value: u64) {
    debug_assert!(value < 1 << 14);
    if value < 1 << 6 {
        buffer.push(value as u8);
    } else {
        buffer.push(0x40 | (value >> 8) as u8);
        buffer.push(value as u8);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wtransport_proto::frame::FrameKind;

    #[test]
    fn the_capsule_parses_back() {
        let payload = close_capsule(0x4805, "going away");
        let frame = Frame::new_data(Cow::Owned(payload));
        assert!(matches!(frame.kind(), FrameKind::Data));

        let capsule = Capsule::with_frame(&frame).expect("a capsule");
        let close = capsules::CloseWebTransportSession::with_capsule(&capsule).expect("parses");
        assert_eq!(close.error_code().into_inner(), 0x4805);
        assert_eq!(close.reason(), "going away");
    }

    #[test]
    fn a_long_reason_is_cut_at_a_character_boundary() {
        let reason = "é".repeat(1000);
        let payload = close_capsule(1, &reason);
        let frame = Frame::new_data(Cow::Owned(payload));
        let capsule = Capsule::with_frame(&frame).expect("a capsule");
        let close = capsules::CloseWebTransportSession::with_capsule(&capsule).expect("parses");
        assert_eq!(close.reason().len(), 1024);
    }
}
