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
pub mod call_resilience;
pub mod datagram;
pub mod fragment;
pub mod jitter;
#[cfg(feature = "message-sounds")]
pub mod message_sounds;
#[cfg(feature = "native")]
pub mod openh264_library;
pub mod picture_check;
pub mod ringtone;
pub mod rtp;
#[cfg(feature = "sound-files")]
pub mod sound_file;
pub mod uplink_layers;
pub mod video;
#[cfg(any(feature = "native", feature = "web"))]
pub mod video_capture;
#[cfg(feature = "native")]
pub mod video_codec;
#[cfg(all(feature = "gstreamer", not(target_os = "android")))]
pub mod video_gstreamer;
#[cfg(feature = "native")]
pub mod video_jpeg;
#[cfg(all(feature = "native", target_os = "android"))]
pub mod video_mediacodec;
#[cfg(feature = "native")]
pub mod video_pipeline;
#[cfg(feature = "native")]
mod worker_thread;

#[cfg(feature = "native")]
pub mod audio_codec;
#[cfg(feature = "native")]
pub mod audio_device;
#[cfg(feature = "native")]
pub mod audio_pipeline;
#[cfg(feature = "native")]
pub mod audio_processing;
#[cfg(feature = "call-auth")]
pub mod call_auth;
#[cfg(feature = "dave")]
pub mod call_cues;
#[cfg(feature = "dave")]
pub mod call_engine;
#[cfg(feature = "native")]
pub mod call_native;
#[cfg(feature = "dave")]
pub mod call_video;
#[cfg(feature = "call-view")]
pub mod call_view;
#[cfg(feature = "web")]
pub mod call_web;
#[cfg(feature = "native")]
pub mod camera_choice;
// Used by the nokhwa cameras (macOS, Windows); tested on every platform.
#[cfg(feature = "native")]
#[cfg_attr(
  not(any(
    all(feature = "avfoundation", target_os = "macos"),
    all(feature = "mediafoundation", target_os = "windows")
  )),
  allow(dead_code)
)]
mod camera_thread;
#[cfg(any(feature = "native", feature = "web"))]
pub mod capture_convert;
#[cfg(feature = "native")]
pub mod capture_fallback;
#[cfg(any(feature = "native", feature = "web"))]
pub mod capture_negotiate;
#[cfg(any(
  all(feature = "avfoundation", target_os = "macos"),
  all(feature = "mediafoundation", target_os = "windows")
))]
pub mod capture_nokhwa;
#[cfg(all(feature = "pipewire", target_os = "linux"))]
pub mod capture_portal;
#[cfg(all(feature = "screencapturekit", target_os = "macos"))]
pub mod capture_screencapturekit;
#[cfg(all(feature = "v4l2", target_os = "linux"))]
pub mod capture_v4l2;
#[cfg(all(feature = "graphics-capture", target_os = "windows"))]
pub mod capture_windows;
#[cfg(feature = "dave")]
pub mod dave;
#[cfg(feature = "effects")]
pub mod effects;
#[cfg(feature = "native")]
mod sfu_address;
#[cfg(feature = "native")]
pub mod system_audio;
#[cfg(all(feature = "pipewire", target_os = "linux"))]
pub mod system_audio_pipewire;
#[cfg(all(feature = "wasapi-loopback", target_os = "windows"))]
pub mod system_audio_wasapi;
#[cfg(feature = "web-api")]
pub mod web_api;
