//! All integration tests of the crate in one test binary: one link step instead of one per file.
//! Each module keeps its own `cfg` gates (features, target), so a module that does not apply
//! here is empty.

mod bandwidth_limits;
mod camera_call;
mod camera_start;
#[path = "../common/mod.rs"]
mod common;
mod dave_call;
mod handover;
mod host_controls;
mod multi_device;
mod native_client;
mod reconnect;
mod reporting;
mod screen_layer_bitrate;
mod screen_sharpness;
mod session_close;
mod sfu_restart;
mod video_call;
mod video_content;
mod video_quality;
mod video_recovery;
mod weak_downlink;
