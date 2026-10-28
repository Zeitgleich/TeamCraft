//! Video over the network: publishing a simulcast track, subscribing, keyframe-gated forwarding of
//! the allocated layer, per-receiver transport sequence numbers and uplink feedback.

// clippy.toml only relaxes these lints inside #[test] functions; the helpers below are test code too.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{
  net::{Ipv6Addr, SocketAddr},
  sync::Arc,
  time::Duration,
};

use teamcraft_media::{
  datagram::{DatagramHeader, MediaDatagram, SimulcastLayer, StreamId, TransportSeq},
  fragment::FrameFragment,
  rtp::{PayloadType, RtpHeader, RtpPacket, RtpTimestamp, SequenceNumber, Ssrc},
};
use teamcraft_proto::{
  CallClientMessage, CallEvent, CallServerMessage, MediaCapabilities, MediaStreamId, TransportFeedback,
  VideoCodec, VideoLayerInfo, VideoPublicationId, VideoTrackKind,
};
use teamcraft_sfu::{
  auth::DevTokenVerifier,
  config::SfuConfig,
  server::{CALL_PATH, Sfu},
  signaling::{read_message, write_message},
};
use wtransport::{ClientConfig, Connection, Endpoint, Identity, RecvStream, SendStream, tls::Sha256Digest};

const CALL_ID: u64 = 7;
const MAX_SIGNALING_BYTES: usize = 1024 * 1024;
const EVENT_TIMEOUT: Duration = Duration::from_secs(5);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscriber_receives_the_allocated_layer_after_a_keyframe() {
  let (port, certificate_hash) = start_sfu().await;
  let mut publisher = Client::join(port, certificate_hash.clone(), 1).await;
  let mut subscriber = Client::join(port, certificate_hash, 2).await;

  publisher
    .send(CallClientMessage::PublishVideo {
      kind: VideoTrackKind::Camera,
      codec: VideoCodec::Av1,
      layers: three_layers(),
      maybe_publication_id: Some(VideoPublicationId(3)),
    })
    .await;
  let published = publisher
    .expect(|event| matches!(event, CallEvent::VideoPublished { .. }))
    .await;
  let CallEvent::VideoPublished { track } = published else {
    unreachable!()
  };
  // The answer names the request it answers (#929).
  assert_eq!(track.maybe_publication_id, Some(VideoPublicationId(3)));
  subscriber
    .expect(|event| matches!(event, CallEvent::VideoPublished { .. }))
    .await;
  let stream_id = track.stream_id;

  subscriber
    .send(CallClientMessage::Subscribe {
      stream_id,
      maybe_max_layer: Some(2),
    })
    .await;
  // The start estimate only allows the lowest layer; the publisher is asked for a keyframe there.
  let request = publisher
    .expect(|event| matches!(event, CallEvent::KeyframeRequested { .. }))
    .await;
  assert_eq!(request, CallEvent::KeyframeRequested { stream_id, layer: 0 });

  // A delta frame first (not forwarded: the receiver has no keyframe yet), then a keyframe on
  // every layer; only layer 0 may arrive.
  publisher.send_frame(stream_id, 0, false, 1).await;
  for layer in 0..3 {
    publisher.send_frame(stream_id, layer, true, 2).await;
  }
  let datagram = tokio::time::timeout(EVENT_TIMEOUT, subscriber.connection.receive_datagram())
    .await
    .expect("no video datagram arrived")
    .unwrap();
  let payload = datagram.payload();
  let parsed = MediaDatagram::parse(&payload).unwrap();
  assert_eq!(parsed.header.stream_id, StreamId(stream_id.0));
  assert_eq!(parsed.header.layer.value(), 0);
  assert_eq!(parsed.header.maybe_transport_seq, Some(TransportSeq(0)));
  let (rtp, _) = RtpHeader::parse(parsed.rtp).unwrap();
  assert!(FrameFragment::from_header(&rtp).unwrap().is_keyframe);
  let maybe_more = tokio::time::timeout(
    Duration::from_millis(300),
    subscriber.connection.receive_datagram(),
  )
  .await;
  assert!(maybe_more.is_err(), "layers 1 and 2 must not be forwarded");

  // Downlink feedback is accepted; the publisher gets feedback on its uplink datagrams.
  subscriber
    .send(CallClientMessage::TransportFeedback {
      feedback: TransportFeedback {
        base_seq: 0,
        arrivals_us: vec![Some(1000)],
      },
    })
    .await;
  let uplink = publisher
    .expect(|event| matches!(event, CallEvent::TransportFeedback { .. }))
    .await;
  let CallEvent::TransportFeedback { feedback } = uplink else {
    unreachable!()
  };
  assert_eq!(
    feedback.arrivals_us.len(),
    4,
    "all four uplink datagrams were numbered 0..=3"
  );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscribing_to_an_undecodable_codec_is_rejected() {
  let (port, certificate_hash) = start_sfu().await;
  let mut publisher = Client::join(port, certificate_hash.clone(), 1).await;
  let mut audio_only = Client::join_with(port, certificate_hash, 2, MediaCapabilities::default()).await;
  publisher
    .send(CallClientMessage::PublishVideo {
      kind: VideoTrackKind::Screen,
      codec: VideoCodec::Av1,
      layers: three_layers()[..1].to_vec(),
      // An older client: nothing to echo.
      maybe_publication_id: None,
    })
    .await;
  let CallEvent::VideoPublished { track } = audio_only
    .expect(|event| matches!(event, CallEvent::VideoPublished { .. }))
    .await
  else {
    unreachable!()
  };
  assert_eq!(track.maybe_publication_id, None);
  audio_only
    .send(CallClientMessage::Subscribe {
      stream_id: track.stream_id,
      maybe_max_layer: Some(0),
    })
    .await;
  let error = audio_only.expect_error().await;
  assert!(error.contains("decode"), "{error}");
}

/// One test client: signaling stream plus the connection for datagrams.
struct Client {
  connection: Connection,
  send: SendStream,
  recv: RecvStream,
  next_transport_seq: TransportSeq,
}

impl Client {
  async fn join(port: u16, certificate_hash: Sha256Digest, device: u64) -> Self {
    let capabilities = MediaCapabilities {
      video_encoders: vec![VideoCodec::Av1],
      video_decoders: vec![VideoCodec::Av1],
      max_simulcast_layers: 3,
      ..MediaCapabilities::default()
    };
    Self::join_with(port, certificate_hash, device, capabilities).await
  }

  async fn join_with(
    port: u16,
    certificate_hash: Sha256Digest,
    device: u64,
    capabilities: MediaCapabilities,
  ) -> Self {
    let config = ClientConfig::builder()
      .with_bind_default()
      .with_server_certificate_hashes([certificate_hash])
      .build();
    let address = SocketAddr::new(Ipv6Addr::LOCALHOST.into(), port);
    let connection = Endpoint::client(config)
      .unwrap()
      .connect(format!("https://{address}{CALL_PATH}"))
      .await
      .unwrap();
    let (send, recv) = connection.open_bi().await.unwrap().await.unwrap();
    let mut client = Self {
      connection,
      send,
      recv,
      next_transport_seq: TransportSeq(0),
    };
    client
      .send(CallClientMessage::Join {
        token: format!("dev:{CALL_ID}:{device}:{device}"),
        capabilities,
      })
      .await;
    client
      .expect(|event| matches!(event, CallEvent::Joined { .. }))
      .await;
    client
  }

  async fn send(&mut self, message: CallClientMessage) {
    write_message(&mut self.send, &message).await.unwrap();
  }

  /// Skips events until one matches.
  async fn expect(&mut self, predicate: impl Fn(&CallEvent) -> bool) -> CallEvent {
    tokio::time::timeout(EVENT_TIMEOUT, async {
      loop {
        match read_message::<CallServerMessage>(&mut self.recv, MAX_SIGNALING_BYTES)
          .await
          .unwrap()
        {
          CallServerMessage::Event { event, .. } if predicate(&event) => return event,
          CallServerMessage::Error { error } => panic!("unexpected error {error:?}"),
          _ => {},
        }
      }
    })
    .await
    .expect("event did not arrive")
  }

  async fn expect_error(&mut self) -> String {
    tokio::time::timeout(EVENT_TIMEOUT, async {
      loop {
        if let CallServerMessage::Error { error } =
          read_message::<CallServerMessage>(&mut self.recv, MAX_SIGNALING_BYTES)
            .await
            .unwrap()
        {
          return error.message;
        }
      }
    })
    .await
    .expect("error did not arrive")
  }

  /// Sends one single-fragment frame on `layer`, numbered for uplink estimation.
  async fn send_frame(&mut self, stream_id: MediaStreamId, layer: u8, is_keyframe: bool, frame_number: u16) {
    let mut header = RtpHeader::new(
      PayloadType::new(100).unwrap(),
      SequenceNumber(frame_number),
      RtpTimestamp(u32::from(frame_number) * 3000),
      Ssrc(stream_id.0),
    );
    FrameFragment {
      frame_number,
      index: 0,
      count: 1,
      is_keyframe,
    }
    .write_to(&mut header);
    let rtp = RtpPacket {
      header,
      payload: &[0x5A; 200],
    }
    .to_bytes()
    .unwrap();
    let datagram = MediaDatagram {
      header: DatagramHeader {
        stream_id: StreamId(stream_id.0),
        layer: SimulcastLayer::new(layer).unwrap(),
        maybe_transport_seq: Some(self.next_transport_seq),
      },
      rtp: &rtp,
    }
    .to_bytes(1200)
    .unwrap();
    self.next_transport_seq = self.next_transport_seq.next();
    self.connection.send_datagram(&datagram).unwrap();
  }
}

async fn start_sfu() -> (u16, Sha256Digest) {
  let config = SfuConfig {
    bind_addr: SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0)),
    ..SfuConfig::default()
  };
  let identity = Identity::self_signed(["localhost", "127.0.0.1", "::1"]).unwrap();
  let sfu = Sfu::bind(config, identity, Arc::new(DevTokenVerifier)).unwrap();
  let port = sfu.local_addr().unwrap().port();
  let certificate_hash = sfu.certificate_hash();
  tokio::spawn(sfu.run());
  (port, certificate_hash)
}

fn three_layers() -> Vec<VideoLayerInfo> {
  [
    (0, 320, 180, 150_000),
    (1, 640, 360, 500_000),
    (2, 1280, 720, 1_500_000),
  ]
  .into_iter()
  .map(|(layer, width, height, max_bitrate_bps)| VideoLayerInfo {
    layer,
    width,
    height,
    max_framerate: 30,
    max_bitrate_bps,
  })
  .collect()
}
