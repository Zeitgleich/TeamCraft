//! RTP packetization, jitter buffer, DAVE frame encryption and simulcast logic.
//!
//! Wire layout of one media datagram (ARCHITECTURE.md §6.1):
//!
//! ```text
//! QUIC datagram = TeamCraft datagram header | RTP header (+ audio-level extension) | DAVE frame
//! ```
//!
//! The SFU reads the first two parts and never touches the DAVE-encrypted payload.

pub mod audio_level;
pub mod datagram;
pub mod jitter;
pub mod rtp;

#[cfg(feature = "native")]
pub mod audio_codec;
#[cfg(feature = "dave")]
pub mod dave;
