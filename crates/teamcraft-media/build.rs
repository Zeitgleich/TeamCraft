//! Declares the build-time switches for WebRTC audio processing (see Cargo.toml and README.md).

fn main() {
  println!("cargo::rustc-check-cfg=cfg(teamcraft_audio_processing)");
  println!("cargo::rustc-check-cfg=cfg(teamcraft_audio_processing_bundled)");
}
