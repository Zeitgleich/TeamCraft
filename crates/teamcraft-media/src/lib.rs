//! RTP packetization, jitter buffer, DAVE frame encryption and simulcast logic.
//!
//! Wire layout of one media datagram (ARCHITECTURE.md §6.1):
//!
//! ```text
//! QUIC datagram = TeamCraft datagram header | RTP header (+ audio-level extension) | DAVE frame
//! ```
//!
//! The SFU reads the first two parts and never touches the DAVE-encrypted payload.

pub mod audio_frame;
pub mod audio_io;
pub mod audio_level;
pub mod bwe;
pub mod datagram;
pub mod fragment;
pub mod jitter;
pub mod rtp;
pub mod video;
#[cfg(any(feature = "native", feature = "web"))]
pub mod video_capture;
#[cfg(feature = "native")]
pub mod video_codec;
#[cfg(feature = "native")]
pub mod video_pipeline;

#[cfg(feature = "native")]
pub mod audio_codec;
#[cfg(feature = "native")]
pub mod audio_device;
#[cfg(feature = "native")]
pub mod audio_pipeline;
#[cfg(feature = "native")]
pub mod audio_processing;
#[cfg(feature = "dave")]
pub mod call_auth;
#[cfg(feature = "dave")]
pub mod call_engine;
#[cfg(feature = "native")]
pub mod call_native;
#[cfg(feature = "dave")]
pub mod call_video;
#[cfg(feature = "web")]
pub mod call_web;
#[cfg(all(feature = "avfoundation", target_os = "macos"))]
pub mod capture_avfoundation;
#[cfg(any(feature = "native", feature = "web"))]
pub mod capture_convert;
#[cfg(all(feature = "pipewire", target_os = "linux"))]
pub mod capture_portal;
#[cfg(all(feature = "v4l2", target_os = "linux"))]
pub mod capture_v4l2;
#[cfg(feature = "dave")]
pub mod dave;
#[cfg(feature = "effects")]
pub mod effects;
#[cfg(feature = "web")]
pub mod web_api;
