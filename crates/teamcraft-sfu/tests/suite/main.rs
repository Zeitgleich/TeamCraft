//! All integration tests of the crate in one test binary: one link step instead of one per file.
//! Each module keeps its own `cfg` gates (features, target), so a module that does not apply
//! here is empty.

mod camera_call;
mod dave_call;
mod handover;
mod host_controls;
mod native_client;
mod reconnect;
mod reporting;
mod video_call;
