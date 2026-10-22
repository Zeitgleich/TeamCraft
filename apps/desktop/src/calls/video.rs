//! Platform choices for the desktop's video: which capture opens the camera or the screen, and
//! the background effects in front of the encoder.
//!
//! Linux: the desktop portal first (asks the user, works on Wayland and in Flatpak), V4L2 for
//! cameras when the portal isn't there. macOS: AVFoundation for cameras (untested); its screen
//! share (ScreenCaptureKit) isn't there yet.

use teamcraft_client_api::calls::BackgroundEffect as UiBackgroundEffect;
use teamcraft_media::effects::{
  BackgroundEffect, BlurStrength, Compositor, EffectHandle, EffectsConfig, EffectsPipeline,
  SegmentationModel, TractSegmenter,
};
use teamcraft_media::video::{VideoResolution, simulcast_layers};
use teamcraft_media::video_capture::{CameraInfo, CaptureFormat, VideoCapture};
use teamcraft_media::video_pipeline::FrameFilter;
use teamcraft_proto::VideoLayerInfo;
use tracing::{info, warn};

/// The ONNX selfie segmentation model (MediaPipe's, landscape) for background effects; without
/// it, effects are off.
const MODEL_VARIABLE: &str = "TEAMCRAFT_SEGMENTATION_MODEL";
const CAMERA_FORMAT: CaptureFormat = CaptureFormat {
  resolution: VideoResolution {
    width: 1280,
    height: 720,
  },
  framerate: 30,
};
const SCREEN_FORMAT: CaptureFormat = CaptureFormat {
  resolution: VideoResolution {
    width: 1920,
    height: 1080,
  },
  framerate: 15,
};
const CAMERA_BITRATE_BPS: u32 = 1_500_000;
const SCREEN_BITRATE_BPS: u32 = 2_000_000;

/// Cameras for the device picker.
#[cfg(target_os = "linux")]
pub(super) fn list_cameras() -> Vec<CameraInfo> {
  teamcraft_media::capture_v4l2::list_cameras()
}

#[cfg(target_os = "macos")]
pub(super) fn list_cameras() -> Vec<CameraInfo> {
  teamcraft_media::capture_avfoundation::list_cameras()
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) fn list_cameras() -> Vec<CameraInfo> {
  Vec::new()
}

/// Opens the camera (blocking: the portal may ask the user). A chosen device is a V4L2 path; the
/// portal only offers "the camera", so it is tried for the default.
#[cfg(target_os = "linux")]
pub(super) fn open_camera(maybe_id: Option<&str>) -> Result<Box<dyn VideoCapture>, String> {
  use teamcraft_media::capture_portal::PortalCapture;
  use teamcraft_media::capture_v4l2::V4l2Camera;

  if maybe_id.is_none() {
    match PortalCapture::camera(CAMERA_FORMAT) {
      Ok(capture) => return Ok(Box::new(capture)),
      Err(error) => info!(%error, "no camera through the portal; trying V4L2"),
    }
  }
  V4l2Camera::open(maybe_id, CAMERA_FORMAT)
    .map(|camera| Box::new(camera) as Box<dyn VideoCapture>)
    .map_err(|error| error.to_string())
}

#[cfg(target_os = "macos")]
pub(super) fn open_camera(maybe_id: Option<&str>) -> Result<Box<dyn VideoCapture>, String> {
  teamcraft_media::capture_avfoundation::NokhwaCamera::open(maybe_id, CAMERA_FORMAT)
    .map(|camera| Box::new(camera) as Box<dyn VideoCapture>)
    .map_err(|error| error.to_string())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) fn open_camera(_maybe_id: Option<&str>) -> Result<Box<dyn VideoCapture>, String> {
  Err("cameras are not supported on this platform yet".into())
}

/// Asks for a screen or window (blocking: the portal shows its picker). Returns the capture and
/// a token that restores the choice next time.
#[cfg(target_os = "linux")]
pub(super) fn open_screen(
  maybe_restore_token: Option<&str>,
) -> Result<(Box<dyn VideoCapture>, Option<String>), String> {
  teamcraft_media::capture_portal::PortalCapture::screen(SCREEN_FORMAT, maybe_restore_token)
    .map(|(capture, maybe_token)| (Box::new(capture) as Box<dyn VideoCapture>, maybe_token))
    .map_err(|error| error.to_string())
}

#[cfg(not(target_os = "linux"))]
pub(super) fn open_screen(
  _maybe_restore_token: Option<&str>,
) -> Result<(Box<dyn VideoCapture>, Option<String>), String> {
  Err("screen sharing is not supported on this platform yet".into())
}

/// Three simulcast layers for a camera.
pub(super) fn camera_layers(format: CaptureFormat) -> Vec<VideoLayerInfo> {
  simulcast_layers(format.resolution, format.framerate, CAMERA_BITRATE_BPS, 3)
}

/// One layer for a screen (no simulcast: text must stay readable).
pub(super) fn screen_layers(format: CaptureFormat) -> Vec<VideoLayerInfo> {
  simulcast_layers(format.resolution, format.framerate, SCREEN_BITRATE_BPS, 1)
}

/// The effects filter for the camera, if a segmentation model and a GPU are available.
pub(super) fn effects_filter(effect: UiBackgroundEffect) -> Option<(FrameFilter, EffectHandle)> {
  let path = std::env::var(MODEL_VARIABLE).ok()?;
  let model = match std::fs::read(&path) {
    Ok(model) => model,
    Err(error) => {
      warn!(%error, %path, "segmentation model not readable; background effects are off");
      return None;
    },
  };
  let segmenter = match TractSegmenter::new(&model, SegmentationModel::MEDIAPIPE_LANDSCAPE) {
    Ok(segmenter) => segmenter,
    Err(error) => {
      warn!(%error, "segmentation model not usable; background effects are off");
      return None;
    },
  };
  let compositor = match pollster::block_on(Compositor::new(EffectsConfig::default())) {
    Ok(compositor) => compositor,
    Err(error) => {
      warn!(%error, "no GPU for background effects");
      return None;
    },
  };
  Some(EffectsPipeline::new(Box::new(segmenter), compositor, media_effect(effect)).into_frame_filter())
}

/// The UI's effect as the media layer's.
pub(super) fn media_effect(effect: UiBackgroundEffect) -> BackgroundEffect {
  match effect {
    UiBackgroundEffect::None => BackgroundEffect::None,
    UiBackgroundEffect::BlurLight => BackgroundEffect::Blur(BlurStrength::Light),
    UiBackgroundEffect::BlurStrong => BackgroundEffect::Blur(BlurStrength::Strong),
    UiBackgroundEffect::Preset(preset) => {
      // The preset images aren't bundled yet; keep the background rather than guess one.
      warn!(?preset, "background images are not available yet");
      BackgroundEffect::None
    },
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn cameras_get_three_layers_and_screens_one() {
    assert_eq!(camera_layers(CAMERA_FORMAT).len(), 3);
    let screen = screen_layers(SCREEN_FORMAT);
    assert_eq!(screen.len(), 1);
    assert_eq!((screen[0].width, screen[0].height), (1920, 1080));
  }

  #[test]
  fn ui_effects_map_to_media_effects() {
    assert_eq!(
      media_effect(UiBackgroundEffect::BlurLight),
      BackgroundEffect::Blur(BlurStrength::Light)
    );
    assert_eq!(media_effect(UiBackgroundEffect::None), BackgroundEffect::None);
  }
}
