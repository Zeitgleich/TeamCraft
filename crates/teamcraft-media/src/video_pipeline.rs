//! The threads between capture and engine: [`VideoSender`] runs capture → (effects) → simulcast
//! encoding, [`VideoReceiver`] decodes one received track for the app. Encoding and decoding take
//! milliseconds per frame, so they stay off the call's async task.

use std::sync::Arc;
use std::sync::mpsc::{self as std_mpsc, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::Duration;

use teamcraft_proto::{VideoLayerInfo, VideoTrackKind};
use tokio::sync::mpsc;
use tracing::warn;

use crate::call_video::ReceivedVideoFrame;
use crate::video::{EncodedVideoFrame, VideoFrame};
use crate::video_capture::{CaptureError, VideoCapture, frame_queue};
use crate::video_codec::{
  OpenH264Decoder, OpenH264Encoder, SimulcastEncoder, VideoCodecError, VideoContent, VideoDecoder,
  VideoEncoder,
};

/// How often the encoder thread looks at controls while no frame arrives.
const CONTROL_POLL: Duration = Duration::from_millis(20);

/// Makes the encoder for one layer; hardware where available (see [`software_encoders`]).
pub type EncoderFactory =
  Arc<dyn Fn(&VideoLayerInfo, VideoContent) -> Result<Box<dyn VideoEncoder>, VideoCodecError> + Send + Sync>;

/// Turns frames before encoding, e.g. background effects. Runs on the encoder thread.
pub type FrameFilter = Box<dyn FnMut(VideoFrame) -> VideoFrame + Send>;

/// Gets decoded frames of a watched track, on the decoder thread.
pub type FrameSink = Box<dyn FnMut(VideoFrame) + Send>;

/// One published track's capture and encoder thread.
pub struct VideoSender {
  pub kind: VideoTrackKind,
  pub layers: Vec<VideoLayerInfo>,
  controls: std_mpsc::Sender<SenderControl>,
  thread: Option<JoinHandle<()>>,
}

/// What to publish and how to encode it.
pub struct VideoSendConfig {
  pub kind: VideoTrackKind,
  pub capture: Box<dyn VideoCapture>,
  /// Simulcast layers, lowest first (one for screen shares).
  pub layers: Vec<VideoLayerInfo>,
  pub encoders: EncoderFactory,
  pub maybe_filter: Option<FrameFilter>,
}

/// One watched track's decoder thread.
pub struct VideoReceiver {
  frames: std_mpsc::Sender<ReceivedVideoFrame>,
  thread: Option<JoinHandle<()>>,
}

enum SenderControl {
  Keyframe(u8),
  NeededLayers(Vec<u8>),
}

/// The software encoders (OpenH264), for platforms without hardware encoding or as a fallback.
pub fn software_encoders() -> EncoderFactory {
  Arc::new(|layer, content| Ok(Box::new(OpenH264Encoder::new(layer, content)?) as Box<dyn VideoEncoder>))
}

impl VideoSender {
  /// Starts capturing and encoding; frames go to `encoded` tagged with the track kind.
  pub fn start(
    config: VideoSendConfig,
    encoded: mpsc::UnboundedSender<(VideoTrackKind, EncodedVideoFrame)>,
  ) -> Result<Self, CaptureError> {
    let VideoSendConfig {
      kind,
      mut capture,
      layers,
      encoders,
      mut maybe_filter,
    } = config;
    let content = match kind {
      VideoTrackKind::Screen => VideoContent::Screen,
      _ => VideoContent::Camera,
    };
    let (frames, frame_rx) = frame_queue();
    capture.start(frames)?;
    let (controls, control_rx) = std_mpsc::channel();
    let thread_layers = layers.clone();
    let thread_kind = kind.clone();
    let thread = std::thread::Builder::new()
      .name("video-encoder".into())
      .spawn(move || {
        let make = |layer: &VideoLayerInfo| encoders(layer, content);
        let mut simulcast = match SimulcastEncoder::new(&thread_layers, make) {
          Ok(simulcast) => simulcast,
          Err(error) => {
            warn!(%error, "no video encoder");
            return;
          },
        };
        loop {
          for control in control_rx.try_iter() {
            match control {
              SenderControl::Keyframe(layer) => {
                if let Err(error) = simulcast.request_keyframe(layer) {
                  warn!(%error, "keyframe request");
                }
              },
              SenderControl::NeededLayers(layers) => simulcast.set_needed_layers(&layers),
            }
          }
          let frame = match frame_rx.recv_timeout(CONTROL_POLL) {
            Ok(frame) => frame,
            Err(RecvTimeoutError::Timeout) => {
              // Stopped: the controls' sender is gone.
              if matches!(control_rx.try_recv(), Err(std_mpsc::TryRecvError::Disconnected)) {
                break;
              }
              continue;
            },
            Err(RecvTimeoutError::Disconnected) => break,
          };
          let frame = match &mut maybe_filter {
            Some(filter) => filter(frame),
            None => frame,
          };
          match simulcast.encode(&frame) {
            Ok(encoded_frames) => {
              for encoded_frame in encoded_frames {
                if encoded.send((thread_kind.clone(), encoded_frame)).is_err() {
                  capture.stop();
                  return;
                }
              }
            },
            Err(error) => warn!(%error, "video frame not encoded"),
          }
        }
        capture.stop();
      })
      .map_err(|error| CaptureError::Device(error.to_string()))?;
    Ok(Self {
      kind,
      layers,
      controls,
      thread: Some(thread),
    })
  }

  pub fn request_keyframe(&self, layer: u8) {
    // A stopped thread needs no keyframe.
    let _ = self.controls.send(SenderControl::Keyframe(layer));
  }

  pub fn set_needed_layers(&self, layers: Vec<u8>) {
    let _ = self.controls.send(SenderControl::NeededLayers(layers));
  }
}

impl Drop for VideoSender {
  fn drop(&mut self) {
    let (closed, _) = std_mpsc::channel();
    drop(std::mem::replace(&mut self.controls, closed));
    if let Some(thread) = self.thread.take()
      && thread.join().is_err()
    {
      warn!("video encoder thread panicked");
    }
  }
}

impl VideoReceiver {
  /// Decodes frames pushed with [`VideoReceiver::push`] into `sink`.
  pub fn start(mut sink: FrameSink) -> Result<Self, VideoCodecError> {
    let (frames, frame_rx) = std_mpsc::channel::<ReceivedVideoFrame>();
    let thread = std::thread::Builder::new()
      .name("video-decoder".into())
      .spawn(move || {
        let mut decoder = match OpenH264Decoder::new() {
          Ok(decoder) => decoder,
          Err(error) => {
            warn!(%error, "no video decoder");
            return;
          },
        };
        let mut is_waiting_for_keyframe = true;
        for frame in frame_rx {
          if is_waiting_for_keyframe && !frame.is_keyframe {
            continue;
          }
          is_waiting_for_keyframe = false;
          // RTP video clock (90 kHz) to microseconds.
          let timestamp_us = u64::from(frame.timestamp.0) * 1_000_000 / 90_000;
          match decoder.decode(&frame.data, timestamp_us) {
            Ok(Some(decoded)) => sink(decoded),
            Ok(None) => {},
            Err(error) => {
              warn!(%error, "video frame not decoded; waiting for a keyframe");
              is_waiting_for_keyframe = true;
            },
          }
        }
      })
      .map_err(|error| VideoCodecError::Codec(error.to_string()))?;
    Ok(Self {
      frames,
      thread: Some(thread),
    })
  }

  pub fn push(&self, frame: ReceivedVideoFrame) {
    // The decoder thread only ends when this receiver is dropped.
    let _ = self.frames.send(frame);
  }
}

impl Drop for VideoReceiver {
  fn drop(&mut self) {
    let (closed, _) = std_mpsc::channel();
    drop(std::mem::replace(&mut self.frames, closed));
    if let Some(thread) = self.thread.take()
      && thread.join().is_err()
    {
      warn!("video decoder thread panicked");
    }
  }
}

#[cfg(test)]
mod tests {
  use std::sync::Mutex;

  use teamcraft_proto::{DeviceId, Id, MediaStreamId, Snowflake, VideoCodec};

  use super::*;
  use crate::video::{VideoResolution, simulcast_layers};
  use crate::video_capture::{CaptureFormat, ManualCapture, test_pattern};

  const VGA: VideoResolution = VideoResolution {
    width: 640,
    height: 360,
  };

  #[tokio::test]
  async fn captured_frames_come_out_encoded_and_decode_again() {
    let capture = ManualCapture::new(CaptureFormat {
      resolution: VGA,
      framerate: 30,
    });
    let (encoded, mut encoded_rx) = mpsc::unbounded_channel();
    let pushed = Arc::new(Mutex::new(None));
    let config = VideoSendConfig {
      kind: VideoTrackKind::Camera,
      capture: Box::new(SharedCapture(capture, Arc::clone(&pushed))),
      layers: simulcast_layers(VGA, 30, 800_000, 2),
      encoders: software_encoders(),
      // Proves the filter runs before encoding: the whole picture turns grey.
      maybe_filter: Some(Box::new(|frame| {
        let resolution = frame.resolution();
        VideoFrame::new(resolution, frame.timestamp_us(), vec![128; frame.data().len()]).unwrap()
      })),
    };
    let sender = VideoSender::start(config, encoded).unwrap();
    let frames = pushed.lock().unwrap().clone().unwrap();
    let decoded = Arc::new(Mutex::new(Vec::new()));
    let sink_frames = Arc::clone(&decoded);
    let receiver =
      VideoReceiver::start(Box::new(move |frame| sink_frames.lock().unwrap().push(frame))).unwrap();

    let mut received = Vec::new();
    for index in 0..10u64 {
      // Wait for room: the queue holds two frames and the encoder takes its time.
      assert!(frames.send(test_pattern(VGA, index, index * 33_333).unwrap()));
      while let Ok(Some((kind, frame))) =
        tokio::time::timeout(Duration::from_millis(200), encoded_rx.recv()).await
      {
        assert_eq!(kind, VideoTrackKind::Camera);
        received.push(frame.layer.value());
        if frame.layer.value() == 1 {
          receiver.push(received_frame(frame));
        }
        if received.len() >= 2 * (index as usize + 1) {
          break;
        }
      }
    }
    sender.request_keyframe(0);
    drop(sender);
    drop(receiver);
    assert!(received.contains(&0) && received.contains(&1), "{received:?}");
    let decoded = decoded.lock().unwrap();
    assert!(!decoded.is_empty());
    let (y, _, _) = decoded[0].planes();
    let average = y.iter().map(|value| u64::from(*value)).sum::<u64>() / y.len() as u64;
    assert!((120..=136).contains(&average), "filtered grey, got {average}");
  }

  /// Lets the test push into a capture the sender owns.
  struct SharedCapture(
    ManualCapture,
    Arc<Mutex<Option<crate::video_capture::FrameSender>>>,
  );

  impl VideoCapture for SharedCapture {
    fn format(&self) -> CaptureFormat {
      self.0.format()
    }

    fn start(&mut self, frames: crate::video_capture::FrameSender) -> Result<(), CaptureError> {
      *self.1.lock().unwrap() = Some(frames.clone());
      self.0.start(frames)
    }

    fn stop(&mut self) {
      self.0.stop();
    }
  }

  fn received_frame(frame: EncodedVideoFrame) -> ReceivedVideoFrame {
    ReceivedVideoFrame {
      sender: DeviceId::from_snowflake(Snowflake::new(1).unwrap()),
      stream_id: MediaStreamId(1),
      codec: VideoCodec::H264,
      layer: frame.layer,
      timestamp: frame.timestamp,
      is_keyframe: frame.is_keyframe,
      data: frame.data,
    }
  }
}
