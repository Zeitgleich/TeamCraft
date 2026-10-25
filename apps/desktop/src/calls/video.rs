//! Platform choices for the desktop's video: which capture opens the camera or the screen, and
//! the background effects in front of the encoder.
//!
//! Linux: the desktop portal first (asks the user, works on Wayland and in Flatpak), V4L2 for
//! cameras when the portal isn't there. macOS: AVFoundation for cameras and ScreenCaptureKit for
//! screens (both untested on a Mac). Windows: Media Foundation for cameras and Windows Graphics
//! Capture for screens and windows (both compile-checked only).
//!
//! A screen share's sound (the computer's playback without the call): PipeWire on Linux, WASAPI
//! process loopback on Windows, ScreenCaptureKit on macOS.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc as std_mpsc;

use teamcraft_client_api::calls::{
  BackgroundEffect as UiBackgroundEffect, BackgroundPreset as UiBackgroundPreset,
};
use teamcraft_media::call_video::ReceivedVideoFrame;
use teamcraft_media::effects::{
  BackgroundEffect, BackgroundImage, BlurStrength, Compositor, EffectHandle, EffectsConfig, EffectsError,
  EffectsPipeline, PRESET_HEIGHT, PRESET_WIDTH, PhotoBackground, PresetBackground, SegmentationModel,
  TractSegmenter,
};
use teamcraft_media::system_audio::SystemAudioCapture;
use teamcraft_media::video::{VideoResolution, simulcast_layers};
use teamcraft_media::video_capture::{CameraInfo, CaptureFormat, VideoCapture};
use teamcraft_media::video_codec::OpenH264Decoder;
use teamcraft_media::video_jpeg::JpegTranscoder;
use teamcraft_media::video_pipeline::FrameFilter;
use teamcraft_proto::{MediaStreamId, VideoCodec, VideoLayerInfo};
use tracing::{debug, info, warn};

use super::VideoSink;

/// Overrides the bundled segmentation model (development: a model fetched with
/// `crates/teamcraft-media/models/fetch.sh`).
const MODEL_VARIABLE: &str = "TEAMCRAFT_SEGMENTATION_MODEL";
/// The bundled model, relative to the app's resources (tauri.models.conf.json).
pub(crate) const BUNDLED_MODEL: &str = "models/selfie_segmentation_landscape.onnx";
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

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub(super) fn list_cameras() -> Vec<CameraInfo> {
  teamcraft_media::capture_nokhwa::list_cameras()
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
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

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub(super) fn open_camera(maybe_id: Option<&str>) -> Result<Box<dyn VideoCapture>, String> {
  teamcraft_media::capture_nokhwa::NokhwaCamera::open(maybe_id, CAMERA_FORMAT)
    .map(|camera| Box::new(camera) as Box<dyn VideoCapture>)
    .map_err(|error| error.to_string())
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
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

/// The primary display, or the source a stored token names (`monitor:N`, `window:Title`).
#[cfg(target_os = "windows")]
pub(super) fn open_screen(
  maybe_restore_token: Option<&str>,
) -> Result<(Box<dyn VideoCapture>, Option<String>), String> {
  use teamcraft_media::capture_windows::{ScreenSource, WindowsScreenCapture};

  let source = ScreenSource::from_token(maybe_restore_token);
  let maybe_token = source.to_token();
  WindowsScreenCapture::open(source, SCREEN_FORMAT)
    .map(|capture| (Box::new(capture) as Box<dyn VideoCapture>, maybe_token))
    .map_err(|error| error.to_string())
}

/// What the user picks in the system's picker each time (a window, an application or a display;
/// never TeamCraft's own windows). The first share asks for the Screen Recording permission. Runs on a blocking thread: the picker waits for the
/// user.
#[cfg(target_os = "macos")]
pub(super) fn open_screen(
  _maybe_restore_token: Option<&str>,
) -> Result<(Box<dyn VideoCapture>, Option<String>), String> {
  use teamcraft_media::capture_screencapturekit::ScreenCaptureKitScreen;

  ScreenCaptureKitScreen::pick(SCREEN_FORMAT)
    .map(|capture| (Box::new(capture) as Box<dyn VideoCapture>, None))
    .map_err(|error| error.to_string())
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
pub(super) fn open_screen(
  _maybe_restore_token: Option<&str>,
) -> Result<(Box<dyn VideoCapture>, Option<String>), String> {
  Err("screen sharing is not supported on this platform yet".into())
}

/// The computer's sound for a screen share, without this app's own playback.
#[cfg(target_os = "linux")]
pub(super) fn open_system_audio() -> Box<dyn SystemAudioCapture> {
  Box::new(teamcraft_media::system_audio_pipewire::PipeWireSystemAudio::new())
}

#[cfg(target_os = "windows")]
pub(super) fn open_system_audio() -> Box<dyn SystemAudioCapture> {
  Box::new(teamcraft_media::system_audio_wasapi::WasapiSystemAudio::new())
}

#[cfg(target_os = "macos")]
pub(super) fn open_system_audio() -> Box<dyn SystemAudioCapture> {
  Box::new(teamcraft_media::capture_screencapturekit::ScreenCaptureKitAudio::new())
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
pub(super) fn open_system_audio() -> Box<dyn SystemAudioCapture> {
  Box::new(teamcraft_media::system_audio::UnsupportedSystemAudio)
}

/// Three simulcast layers for a camera.
pub(super) fn camera_layers(format: CaptureFormat) -> Vec<VideoLayerInfo> {
  simulcast_layers(format.resolution, format.framerate, CAMERA_BITRATE_BPS, 3)
}

/// One layer for a screen (no simulcast: text must stay readable).
pub(super) fn screen_layers(format: CaptureFormat) -> Vec<VideoLayerInfo> {
  simulcast_layers(format.resolution, format.framerate, SCREEN_BITRATE_BPS, 1)
}

/// The effects filter for the camera, if a segmentation model and a GPU are available. The model
/// comes from `TEAMCRAFT_SEGMENTATION_MODEL`, else from the app's resources.
pub(super) fn effects_filter(
  effect: UiBackgroundEffect,
  maybe_custom: Option<&Arc<BackgroundImage>>,
  maybe_bundled_model: Option<&Path>,
) -> Option<(FrameFilter, EffectHandle)> {
  let path = model_path(maybe_bundled_model)?;
  let path = path.display().to_string();
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
  Some(
    EffectsPipeline::new(
      Box::new(segmenter),
      compositor,
      media_effect(effect, maybe_custom),
    )
    .into_frame_filter(),
  )
}

fn model_path(maybe_bundled_model: Option<&Path>) -> Option<PathBuf> {
  if let Ok(path) = std::env::var(MODEL_VARIABLE) {
    return Some(PathBuf::from(path));
  }
  let bundled = maybe_bundled_model.filter(|path| path.exists());
  if bundled.is_none() {
    info!("no segmentation model bundled; background effects are off");
  }
  bundled.map(Path::to_path_buf)
}

const PREVIEW_JPEG_QUALITY: u8 = 80;

/// Version of the frame message the webview decodes (see [`video_message`]).
const VIDEO_MESSAGE_VERSION: u8 = 1;
const KEYFRAME_FLAG: u8 = 0x01;
const JPEG_FLAG: u8 = 0x02;
const CODEC_H264: u8 = 1;
const CODEC_VP9: u8 = 2;
const CODEC_JPEG: u8 = 3;

/// Sends a watched track's frames as they arrive, for the webview's WebCodecs decoder.
pub(super) fn h264_sink(send: impl Fn(Vec<u8>) + Send + Sync + 'static) -> VideoSink {
  Arc::new(move |frame| send(video_message(frame)))
}

/// Decodes a watched track on a thread of its own and sends JPEG images at a reduced rate, for
/// webviews without WebCodecs. The thread ends when the sink is dropped (unwatch, call end).
pub(super) fn jpeg_sink(send: impl Fn(Vec<u8>) + Send + 'static) -> VideoSink {
  let (frames, queue) = std_mpsc::channel::<ReceivedVideoFrame>();
  let spawned = std::thread::Builder::new()
    .name("teamcraft-video-jpeg".into())
    .spawn(move || transcode_to_jpeg(&queue, &send));
  if let Err(error) = spawned {
    warn!(%error, "no thread for JPEG video; the track stays blank");
  }
  // Unbounded on purpose: dropping H.264 frames would break decoding until the next keyframe.
  // The sender only fails once the thread is gone; then there is nobody to decode for.
  Arc::new(move |frame| drop(frames.send(frame.clone())))
}

fn transcode_to_jpeg(queue: &std_mpsc::Receiver<ReceivedVideoFrame>, send: &dyn Fn(Vec<u8>)) {
  let decoder = match OpenH264Decoder::new() {
    Ok(decoder) => decoder,
    Err(error) => {
      warn!(%error, "no H.264 decoder for JPEG video");
      return;
    },
  };
  let mut transcoder = JpegTranscoder::new(Box::new(decoder));
  while let Ok(frame) = queue.recv() {
    let timestamp_us = timestamp_us(&frame);
    match transcoder.push(&frame.data, timestamp_us) {
      Ok(Some(jpeg)) => send(jpeg_message(frame.stream_id, timestamp_us, &jpeg)),
      Ok(None) => {},
      // A broken frame: later ones recover with the next keyframe.
      Err(error) => debug!(%error, "frame not decodable"),
    }
  }
}

/// One received frame for the webview, as a single binary channel message:
/// `[version u8][flags u8: bit 0 keyframe, bit 1 JPEG][codec u8: 1 H.264, 2 VP9, 3 JPEG, 0 other]`
/// `[reserved u8][stream id u32 BE][timestamp µs u64 BE]` and then the payload (H.264 in Annex B,
/// or one complete JPEG image).
pub(super) fn video_message(frame: &ReceivedVideoFrame) -> Vec<u8> {
  let codec = match frame.codec {
    VideoCodec::H264 => CODEC_H264,
    VideoCodec::Vp9 => CODEC_VP9,
    _ => 0,
  };
  let flags = if frame.is_keyframe { KEYFRAME_FLAG } else { 0 };
  message(flags, codec, frame.stream_id, timestamp_us(frame), &frame.data)
}

/// One complete JPEG image in the format of [`video_message`] (a keyframe: it stands alone).
pub(super) fn jpeg_message(stream_id: MediaStreamId, timestamp_us: u64, jpeg: &[u8]) -> Vec<u8> {
  message(
    KEYFRAME_FLAG | JPEG_FLAG,
    CODEC_JPEG,
    stream_id,
    timestamp_us,
    jpeg,
  )
}

fn message(flags: u8, codec: u8, stream_id: MediaStreamId, timestamp_us: u64, payload: &[u8]) -> Vec<u8> {
  let mut message = Vec::with_capacity(16 + payload.len());
  message.extend_from_slice(&[VIDEO_MESSAGE_VERSION, flags, codec, 0]);
  message.extend_from_slice(&stream_id.0.to_be_bytes());
  message.extend_from_slice(&timestamp_us.to_be_bytes());
  message.extend_from_slice(payload);
  message
}

/// RTP video clock (90 kHz) to microseconds.
fn timestamp_us(frame: &ReceivedVideoFrame) -> u64 {
  u64::from(frame.timestamp.0) * 1_000_000 / 90_000
}

/// The UI's effect as the media layer's; `maybe_custom` is the picture for
/// [`UiBackgroundEffect::Custom`] or [`UiBackgroundEffect::Org`], already fetched and decoded.
pub(super) fn media_effect(
  effect: UiBackgroundEffect,
  maybe_custom: Option<&Arc<BackgroundImage>>,
) -> BackgroundEffect {
  match effect {
    UiBackgroundEffect::None => BackgroundEffect::None,
    UiBackgroundEffect::BlurLight => BackgroundEffect::Blur(BlurStrength::Light),
    UiBackgroundEffect::BlurStrong => BackgroundEffect::Blur(BlurStrength::Strong),
    UiBackgroundEffect::Preset(preset) => match preset_image(preset, PRESET_WIDTH, PRESET_HEIGHT) {
      Ok(image) => BackgroundEffect::Image(Arc::new(image)),
      // Only an empty size or a broken built-in file fails; keep the real background.
      Err(error) => {
        warn!(%error, ?preset, "preset background not loaded");
        BackgroundEffect::None
      },
    },
    UiBackgroundEffect::Custom(_) | UiBackgroundEffect::Org(_) => match maybe_custom {
      Some(image) => BackgroundEffect::Image(Arc::clone(image)),
      None => {
        warn!("own background picture not loaded; keeping the real background");
        BackgroundEffect::None
      },
    },
  }
}

/// A picture file of the user's (JPEG or PNG) ready for the compositor: decoded and scaled to the
/// preset size (calls send at most 720p). Blocking: decoding takes a moment.
pub(crate) fn decode_background(picture: &[u8]) -> Result<Arc<BackgroundImage>, String> {
  BackgroundImage::decode(picture)
    .map(|image| Arc::new(image.fit_within(PRESET_WIDTH, PRESET_HEIGHT)))
    .map_err(|error| error.to_string())
}

/// A small JPEG of a background for the picker: presets are drawn at that size, the user's own
/// pictures are scaled down.
pub(crate) fn background_preview(
  effect: UiBackgroundEffect,
  maybe_custom: Option<&[u8]>,
  width: u32,
  height: u32,
) -> Result<Vec<u8>, String> {
  let image = match (effect, maybe_custom) {
    (UiBackgroundEffect::Preset(preset), _) => preset_image(preset, width, height),
    (UiBackgroundEffect::Custom(_) | UiBackgroundEffect::Org(_), Some(picture)) => {
      BackgroundImage::decode(picture).map(|image| image.fit_within(width, height))
    },
    _ => return Err("only presets and own pictures have a preview".into()),
  };
  image
    .and_then(|image| image.to_jpeg(PREVIEW_JPEG_QUALITY))
    .map_err(|error| error.to_string())
}

/// A preset at (at most) `width` x `height`: a photo decoded from its compiled-in file, an
/// illustration drawn at exactly that size.
fn preset_image(
  preset: UiBackgroundPreset,
  width: u32,
  height: u32,
) -> Result<BackgroundImage, EffectsError> {
  match photo_background(preset) {
    Some(photo) => photo.decode(width, height),
    None => illustration(preset).render(width, height),
  }
}

/// The photo behind a UI preset, if it is one (the UI and the media layer share the slugs).
fn photo_background(preset: UiBackgroundPreset) -> Option<PhotoBackground> {
  PhotoBackground::from_slug(preset.slug())
}

/// The UI's illustration preset as the media layer's drawing of it.
fn illustration(preset: UiBackgroundPreset) -> PresetBackground {
  match preset {
    UiBackgroundPreset::Bookshelf => PresetBackground::Bookshelf,
    UiBackgroundPreset::Sunrise => PresetBackground::Sunrise,
    UiBackgroundPreset::Forest => PresetBackground::Forest,
    // Every other preset is a photo (checked by a test); the office is the neutral fallback.
    _ => PresetBackground::Office,
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
  fn video_messages_have_a_fixed_header() {
    use teamcraft_media::datagram::SimulcastLayer;
    use teamcraft_media::rtp::RtpTimestamp;
    use teamcraft_proto::{DeviceId, Id, MediaStreamId, Snowflake};

    let frame = ReceivedVideoFrame {
      sender: DeviceId::from_snowflake(Snowflake::new(2).unwrap()),
      stream_id: MediaStreamId(0x0102_0304),
      codec: VideoCodec::H264,
      layer: SimulcastLayer::BASE,
      timestamp: RtpTimestamp(90_000),
      is_keyframe: true,
      data: vec![0, 0, 0, 1, 0x65],
    };
    assert_eq!(
      video_message(&frame),
      vec![
        1, 1, 1, 0, 1, 2, 3, 4, 0, 0, 0, 0, 0, 0x0f, 0x42, 0x40, 0, 0, 0, 1, 0x65
      ]
    );
  }

  #[test]
  fn jpeg_sinks_send_decoded_images() {
    use std::time::Duration;

    use teamcraft_media::datagram::SimulcastLayer;
    use teamcraft_media::rtp::RtpTimestamp;
    use teamcraft_media::video_capture::test_pattern;
    use teamcraft_media::video_codec::{OpenH264Encoder, VideoContent, VideoEncoder};
    use teamcraft_proto::{DeviceId, Id, MediaStreamId, Snowflake};

    let layer = screen_layers(CaptureFormat {
      resolution: VideoResolution {
        width: 320,
        height: 180,
      },
      framerate: 15,
    })[0];
    let mut encoder = OpenH264Encoder::new(&layer, VideoContent::Camera).unwrap();
    let picture = encoder
      .encode(&test_pattern(layer_resolution(&layer), 0, 0).unwrap(), true)
      .unwrap()
      .unwrap();
    let (images, received) = std_mpsc::channel();
    let sink = jpeg_sink(move |message| drop(images.send(message)));
    sink(&ReceivedVideoFrame {
      sender: DeviceId::from_snowflake(Snowflake::new(2).unwrap()),
      stream_id: MediaStreamId(7),
      codec: VideoCodec::H264,
      layer: SimulcastLayer::BASE,
      timestamp: RtpTimestamp(0),
      is_keyframe: true,
      data: picture.data,
    });

    let message = received.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(&message[..4], &[1, KEYFRAME_FLAG | JPEG_FLAG, CODEC_JPEG, 0]);
    assert_eq!(&message[16..18], &[0xff, 0xd8]);
  }

  #[test]
  fn own_pictures_and_presets_get_previews() {
    let photo = BackgroundImage::new(1600, 1200, vec![90; 1600 * 1200 * 4])
      .unwrap()
      .to_jpeg(80)
      .unwrap();
    let custom = UiBackgroundEffect::Custom("7".parse().unwrap());
    let preview = background_preview(custom, Some(&photo), 160, 90).unwrap();
    let decoded = BackgroundImage::decode(&preview).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (120, 90));
    let preset = background_preview(
      UiBackgroundEffect::Preset(UiBackgroundPreset::Office),
      None,
      160,
      90,
    )
    .unwrap();
    assert_eq!(BackgroundImage::decode(&preset).unwrap().width(), 160);
    assert!(background_preview(UiBackgroundEffect::BlurLight, None, 160, 90).is_err());

    // In a call the picture is scaled to the compositor's size and used as the background.
    let image = decode_background(&photo).unwrap();
    assert_eq!((image.width(), image.height()), (960, 720));
    assert!(matches!(
      media_effect(custom, Some(&image)),
      BackgroundEffect::Image(_)
    ));
    assert_eq!(media_effect(custom, None), BackgroundEffect::None);
  }

  #[test]
  fn ui_effects_map_to_media_effects() {
    assert_eq!(
      media_effect(UiBackgroundEffect::BlurLight, None),
      BackgroundEffect::Blur(BlurStrength::Light)
    );
    assert_eq!(
      media_effect(UiBackgroundEffect::None, None),
      BackgroundEffect::None
    );
    assert!(matches!(
      media_effect(UiBackgroundEffect::Preset(UiBackgroundPreset::Sunrise), None),
      BackgroundEffect::Image(image) if image.width() == PRESET_WIDTH
    ));
  }

  #[test]
  fn photo_presets_load_at_call_size_and_illustrations_are_drawn() {
    for preset in UiBackgroundPreset::PHOTOS {
      assert!(photo_background(preset).is_some(), "{}", preset.slug());
      assert!(matches!(
        media_effect(UiBackgroundEffect::Preset(preset), None),
        BackgroundEffect::Image(image) if (image.width(), image.height()) == (PRESET_WIDTH, PRESET_HEIGHT)
      ));
    }
    for preset in UiBackgroundPreset::ILLUSTRATIONS {
      assert!(photo_background(preset).is_none(), "{}", preset.slug());
    }
    let preview = background_preview(
      UiBackgroundEffect::Preset(UiBackgroundPreset::LivingRoom),
      None,
      160,
      90,
    )
    .unwrap();
    let decoded = BackgroundImage::decode(&preview).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (160, 90));
  }

  fn layer_resolution(layer: &VideoLayerInfo) -> VideoResolution {
    VideoResolution {
      width: layer.width,
      height: layer.height,
    }
  }
}
