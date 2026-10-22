//! End to end with the real native client: three headless clients join through this SFU, form the
//! DAVE group with device-bound keys (call key bindings), send Opus test tones and decode each other.

// clippy.toml only relaxes these lints inside #[test] functions; the helpers below are test code too.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{
  collections::HashMap,
  net::{Ipv6Addr, SocketAddr},
  sync::{Arc, Mutex},
  time::Duration,
};

use rand_core::OsRng;
use teamcraft_crypto::{
  identity::{DeviceKeyPair, IdentityKeyPair},
  ids::{DeviceId as CryptoDeviceId, UserId as CryptoUserId},
};
use teamcraft_media::{
  audio_device::AudioDeviceError,
  audio_io::{AudioDeviceChoice, AudioDeviceId, AudioSink, AudioSource, sine_tone},
  bwe::BweConfig,
  call_auth::{CallKeyAuthenticator, DeviceCallKeyAuthenticator},
  call_engine::{CallEngineConfig, CallEngineEvent},
  call_native::{NativeCall, NativeCallConfig, NativeCallError},
  datagram::SimulcastLayer,
  rtp::RtpTimestamp,
  system_audio::{SystemAudioCapture, SystemAudioError, SystemAudioSender},
  video::{EncodedVideoFrame, VideoResolution, simulcast_layers},
};
use teamcraft_proto::{AudioCodec, DeviceId, MediaCapabilities, MediaStreamId, VideoCodec, VideoTrackKind};
use teamcraft_sfu::{
  auth::DevTokenVerifier,
  config::SfuConfig,
  server::{CALL_PATH, Sfu},
};
use wtransport::Identity;

const CALL_ID: u64 = 11;
const DEVICES: [u64; 3] = [1, 2, 3];
const CALL_DURATION: Duration = Duration::from_secs(3);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn headless_clients_hear_each_other_through_the_sfu() {
  let (port, certificate_hash) = start_sfu().await;
  let mut authenticators = authenticators();
  let mut calls = Vec::new();
  for (index, device) in DEVICES.into_iter().enumerate() {
    let roster: Vec<DeviceId> = DEVICES
      .iter()
      .filter(|other| **other != device)
      .map(|id| id_of(*id))
      .collect();
    let engine = CallEngineConfig {
      user_id: id_of(device),
      device_id: id_of(device),
      capabilities: MediaCapabilities::default(),
      bwe: BweConfig::default(),
    };
    let sfu_url = format!(
      "https://{}{CALL_PATH}",
      SocketAddr::new(Ipv6Addr::LOCALHOST.into(), port)
    );
    let mut config = NativeCallConfig::new(sfu_url, format!("dev:{CALL_ID}:{device}:{device}"), engine);
    config.maybe_certificate_hash = Some(certificate_hash);
    config.source = AudioSource::Tone {
      frequency_hz: 300.0 + 200.0 * index as f32,
      amplitude: 0.3,
    };
    config.sink = AudioSink::Discard;
    config.maybe_trusted_roster = Some(roster);
    config.maybe_authenticator = authenticators.remove(&device);
    calls.push(NativeCall::join(config).await.unwrap());
  }

  let mut secured = vec![false; calls.len()];
  let deadline = tokio::time::Instant::now() + CALL_DURATION;
  while tokio::time::Instant::now() < deadline {
    for (index, call) in calls.iter_mut().enumerate() {
      if let Ok(Some(event)) = tokio::time::timeout(Duration::from_millis(10), call.next_event()).await {
        if matches!(event, CallEngineEvent::Secured { .. }) {
          secured[index] = true;
        }
        assert!(
          !matches!(
            event,
            CallEngineEvent::UntrustedRoster | CallEngineEvent::Error(_)
          ),
          "{event:?}"
        );
      }
    }
  }

  let mut codes = Vec::new();
  for call in calls {
    let stats = call.leave().await.unwrap();
    // Two others for most of the call: well above one second of frames.
    assert!(stats.frames_received > 100, "{stats:?}");
    assert!(stats.audible_frames_played > 50, "{stats:?}");
    assert!(stats.frames_sent > 100, "{stats:?}");
    codes.push(stats.maybe_privacy_code.unwrap());
  }
  assert!(secured.iter().all(|is_secured| *is_secured), "{secured:?}");
  assert!(codes.windows(2).all(|pair| pair[0] == pair[1]), "{codes:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn audio_switches_mid_call_and_a_missing_device_keeps_the_old_one() {
  let (port, certificate_hash) = start_sfu().await;
  let engine = CallEngineConfig {
    user_id: id_of(9),
    device_id: id_of(9),
    capabilities: MediaCapabilities::default(),
    bwe: BweConfig::default(),
  };
  let sfu_url = format!("https://[::1]:{port}{CALL_PATH}");
  let mut config = NativeCallConfig::new(sfu_url, "dev:12:9:9".into(), engine);
  config.maybe_certificate_hash = Some(certificate_hash);
  let call = NativeCall::join(config).await.unwrap();

  let missing = AudioDeviceChoice::Device(AudioDeviceId::new("no-such-host:no-such-device"));
  let result = call
    .set_audio(AudioSource::Microphone(missing), AudioSink::Discard)
    .await;
  assert!(
    matches!(
      result,
      Err(NativeCallError::Device(AudioDeviceError::DeviceGone("input")))
    ),
    "{result:?}"
  );
  let tone = AudioSource::Tone {
    frequency_hz: 440.0,
    amplitude: 0.3,
  };
  call.set_audio(tone, AudioSink::Discard).await.unwrap();
  tokio::time::sleep(Duration::from_millis(500)).await;

  let stats = call.leave().await.unwrap();
  // Silence sends nothing, so every frame came from the tone switched in mid-call.
  assert!(stats.frames_sent > 10, "{stats:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn screen_share_reaches_a_subscriber_through_the_sfu() {
  let (port, certificate_hash) = start_sfu().await;
  let mut authenticators = authenticators();
  let [presenter_device, viewer_device] = [1u64, 2];
  let mut calls = Vec::new();
  for (device, other) in [
    (presenter_device, viewer_device),
    (viewer_device, presenter_device),
  ] {
    let engine = CallEngineConfig {
      user_id: id_of(device),
      device_id: id_of(device),
      capabilities: MediaCapabilities {
        audio_codecs: vec![AudioCodec::Opus],
        video_encoders: vec![VideoCodec::Vp9],
        video_decoders: vec![VideoCodec::Vp9],
        max_simulcast_layers: 1,
      },
      bwe: BweConfig::default(),
    };
    let sfu_url = format!("https://[::1]:{port}{CALL_PATH}");
    let mut config = NativeCallConfig::new(sfu_url, format!("dev:13:{device}:{device}"), engine);
    config.maybe_certificate_hash = Some(certificate_hash);
    config.maybe_trusted_roster = Some(vec![id_of(other)]);
    config.maybe_authenticator = authenticators.remove(&device);
    calls.push(NativeCall::join(config).await.unwrap());
  }
  let [mut presenter, mut viewer]: [NativeCall; 2] = calls.try_into().ok().unwrap();

  // Simulcast off for screen shares: one full-resolution layer.
  let layers = simulcast_layers(
    VideoResolution {
      width: 1920,
      height: 1080,
    },
    15,
    1_500_000,
    1,
  );
  assert_eq!(layers.len(), 1);
  let mut maybe_stream: Option<MediaStreamId> = None;
  let mut is_presenter_secured = false;
  let mut has_published = false;
  let mut wants_keyframe = true;
  let mut frame_index = 0u32;
  let mut received: Vec<EncodedFrameCheck> = Vec::new();
  let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
  while tokio::time::Instant::now() < deadline && received.len() < 10 {
    if let Ok(Some(event)) = tokio::time::timeout(Duration::from_millis(5), presenter.next_event()).await {
      match event {
        CallEngineEvent::Secured { .. } => is_presenter_secured = true,
        CallEngineEvent::VideoPublished(track) if track.device_id == id_of(presenter_device) => {
          maybe_stream = Some(track.stream_id);
        },
        CallEngineEvent::KeyframeRequested { .. } => wants_keyframe = true,
        _ => {},
      }
    }
    if let Ok(Some(event)) = tokio::time::timeout(Duration::from_millis(5), viewer.next_event()).await {
      match event {
        CallEngineEvent::VideoPublished(track) => {
          assert_eq!(track.kind, VideoTrackKind::Screen);
          viewer.subscribe_video(track.stream_id, Some(0));
        },
        CallEngineEvent::VideoFrame(frame) => received.push(EncodedFrameCheck {
          is_keyframe: frame.is_keyframe,
          is_intact: frame.data == screen_frame(u32::from_be_bytes(frame.data[..4].try_into().unwrap())),
        }),
        _ => {},
      }
    }
    if is_presenter_secured && !has_published {
      presenter.publish_video(VideoTrackKind::Screen, VideoCodec::Vp9, layers.clone());
      has_published = true;
    }
    if let Some(stream_id) = maybe_stream {
      presenter.send_video(
        stream_id,
        EncodedVideoFrame {
          codec: VideoCodec::Vp9,
          layer: SimulcastLayer::BASE,
          timestamp: RtpTimestamp(frame_index * 6000),
          is_keyframe: std::mem::take(&mut wants_keyframe),
          data: screen_frame(frame_index),
        },
      );
      frame_index += 1;
    }
  }
  presenter.leave().await.unwrap();
  viewer.leave().await.unwrap();
  assert!(received.len() >= 10, "{} frames arrived", received.len());
  assert!(received[0].is_keyframe, "the SFU starts a receiver at a keyframe");
  assert!(received.iter().all(|frame| frame.is_intact));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shared_screen_sound_reaches_the_others_while_the_microphone_is_muted() {
  let (port, certificate_hash) = start_sfu().await;
  let mut authenticators = authenticators();
  let [presenter_device, listener_device] = [1u64, 2];
  let mut calls = Vec::new();
  for (device, other) in [
    (presenter_device, listener_device),
    (listener_device, presenter_device),
  ] {
    let engine = CallEngineConfig {
      user_id: id_of(device),
      device_id: id_of(device),
      capabilities: MediaCapabilities::default(),
      bwe: BweConfig::default(),
    };
    let sfu_url = format!("https://[::1]:{port}{CALL_PATH}");
    let mut config = NativeCallConfig::new(sfu_url, format!("dev:14:{device}:{device}"), engine);
    config.maybe_certificate_hash = Some(certificate_hash);
    config.maybe_trusted_roster = Some(vec![id_of(other)]);
    config.maybe_authenticator = authenticators.remove(&device);
    calls.push(NativeCall::join(config).await.unwrap());
  }
  let [mut presenter, mut listener]: [NativeCall; 2] = calls.try_into().ok().unwrap();
  presenter.set_muted(true);
  let capture = SharedCapture::default();

  // Both sides learn that the presenter shares sound once the SFU assigned the stream.
  wait_for(&mut presenter, |event| {
    matches!(event, CallEngineEvent::Secured { .. })
  })
  .await;
  presenter
    .start_screen_audio(Box::new(capture.clone()))
    .await
    .unwrap();
  let shared = CallEngineEvent::ScreenAudioChanged {
    device_id: id_of(presenter_device),
    is_shared: true,
  };
  wait_for(&mut presenter, |event| *event == shared).await;
  wait_for(&mut listener, |event| *event == shared).await;

  // Two seconds of a tone at the pace of a real device: sent although the microphone is muted.
  let sender = capture.sender().expect("the capture was started");
  let mut pace = tokio::time::interval(Duration::from_millis(20));
  for frame in 0..100u64 {
    pace.tick().await;
    assert!(sender.send(sine_tone(440.0, 0.3, frame * 960, 960)));
  }
  presenter.stop_screen_audio();
  wait_for(&mut listener, |event| {
    *event
      == CallEngineEvent::ScreenAudioChanged {
        device_id: id_of(presenter_device),
        is_shared: false,
      }
  })
  .await;
  assert!(capture.sender().is_none(), "stopping the share stops the capture");

  presenter.leave().await.unwrap();
  let stats = listener.leave().await.unwrap();
  // The muted microphone sent nothing, so every received frame is screen sound.
  assert!(stats.frames_received > 80, "{stats:?}");
  assert!(stats.audible_frames_played > 50, "{stats:?}");
}

/// A system audio capture the test feeds by hand.
#[derive(Clone, Default)]
struct SharedCapture(Arc<Mutex<Option<SystemAudioSender>>>);

impl SharedCapture {
  fn sender(&self) -> Option<SystemAudioSender> {
    self.0.lock().unwrap().clone()
  }
}

impl SystemAudioCapture for SharedCapture {
  fn start(&mut self, samples: SystemAudioSender) -> Result<(), SystemAudioError> {
    *self.0.lock().unwrap() = Some(samples);
    Ok(())
  }

  fn stop(&mut self) {
    *self.0.lock().unwrap() = None;
  }
}

/// The next event matching `predicate` within five seconds.
async fn wait_for(call: &mut NativeCall, predicate: impl Fn(&CallEngineEvent) -> bool) -> CallEngineEvent {
  tokio::time::timeout(Duration::from_secs(5), async {
    loop {
      let event = call.next_event().await.expect("call ended before the event");
      if predicate(&event) {
        return event;
      }
    }
  })
  .await
  .expect("event did not arrive")
}

/// What the viewer saw of one frame.
struct EncodedFrameCheck {
  is_keyframe: bool,
  is_intact: bool,
}

/// A 4 KB "encoded" frame (several fragments) that starts with its index.
fn screen_frame(index: u32) -> Vec<u8> {
  let mut data = index.to_be_bytes().to_vec();
  data.extend((0..4000u32).map(|i| (i.wrapping_mul(31) ^ index) as u8));
  data
}

/// One identity per device; every client trusts all device certificates of the call.
fn authenticators() -> HashMap<u64, Arc<dyn CallKeyAuthenticator>> {
  let mut key_pairs = Vec::new();
  let mut verified = HashMap::new();
  for device in DEVICES {
    let identity = IdentityKeyPair::generate(&mut OsRng);
    let user = CryptoUserId::new(1000 + device);
    let key_pair = DeviceKeyPair::generate(user, CryptoDeviceId::new(device), &mut OsRng);
    let certificate = identity.certify_device(user, &key_pair.public_keys()).unwrap();
    verified.insert(
      id_of::<DeviceId>(device),
      certificate.verify(&identity.public_key()).unwrap(),
    );
    key_pairs.push((device, key_pair));
  }
  key_pairs
    .into_iter()
    .map(|(device, key_pair)| {
      let authenticator: Arc<dyn CallKeyAuthenticator> = Arc::new(DeviceCallKeyAuthenticator::new(
        Arc::new(key_pair),
        verified.clone(),
      ));
      (device, authenticator)
    })
    .collect()
}

async fn start_sfu() -> (u16, [u8; 32]) {
  let config = SfuConfig {
    bind_addr: SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0)),
    ..SfuConfig::default()
  };
  let identity = Identity::self_signed(["localhost", "127.0.0.1", "::1"]).unwrap();
  let sfu = Sfu::bind(config, identity, Arc::new(DevTokenVerifier)).unwrap();
  let port = sfu.local_addr().unwrap().port();
  let certificate_hash = *sfu.certificate_hash().as_ref();
  tokio::spawn(sfu.run());
  (port, certificate_hash)
}

fn id_of<T: std::str::FromStr>(raw: u64) -> T
where
  T::Err: std::fmt::Debug,
{
  raw.to_string().parse().unwrap()
}
