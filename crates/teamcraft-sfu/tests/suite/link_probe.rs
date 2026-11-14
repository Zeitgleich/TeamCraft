//! Measuring a link before joining (#1959) against a running SFU over QUIC: a probe ticket opens a
//! session that answers the client's padding and measures its downlink, once; join tokens and
//! probe tickets don't stand in for each other.

// clippy.toml only relaxes these lints inside #[test] functions; the helpers below are test code too.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::{Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use teamcraft_media::datagram::{DatagramHeader, TransportSeq, padding_datagram};
use teamcraft_proto::{
  CallClientMessage, CallEvent, CallServerMessage, MediaCapabilities, TransportFeedback,
};
use teamcraft_sfu::auth::DevTokenVerifier;
use teamcraft_sfu::config::SfuConfig;
use teamcraft_sfu::server::{CALL_PATH, Sfu};
use teamcraft_sfu::signaling::{read_message, write_message};
use wtransport::stream::{RecvStream, SendStream};
use wtransport::{ClientConfig, Connection, Endpoint, Identity};

const DEADLINE: Duration = Duration::from_secs(10);
const MAX_MESSAGE_BYTES: usize = 64 * 1024;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_ticket_measures_both_ways_once() {
  let (address, hash) = start_sfu();
  let (connection, mut send, mut recv) = open(address, hash).await;
  let probe = CallClientMessage::Probe {
    ticket: "dev-probe:3:42:t1".into(),
  };
  write_message(&mut send, &probe).await.unwrap();
  for seq in 0..5u16 {
    connection
      .send_datagram(padding_datagram(1000, TransportSeq(seq)))
      .unwrap();
  }

  let measured = tokio::time::timeout(DEADLINE, async {
    let (mut has_uplink_feedback, mut maybe_downlink) = (false, None);
    loop {
      tokio::select! {
        datagram = connection.receive_datagram() => {
          // The SFU's padding: report its arrival as a client does.
          let datagram = datagram.unwrap();
          let header = DatagramHeader::parse(&datagram.payload()).unwrap();
          let seq = header.maybe_transport_seq.unwrap();
          let feedback = TransportFeedback { base_seq: seq.0, arrivals_us: vec![Some(u64::from(seq.0) * 4000)] };
          write_message(&mut send, &CallClientMessage::TransportFeedback { feedback }).await.unwrap();
        },
        message = read_message::<CallServerMessage>(&mut recv, MAX_MESSAGE_BYTES) => match message.unwrap() {
          CallServerMessage::Event { event: CallEvent::TransportFeedback { feedback }, .. } => {
            assert!(feedback.arrivals_us.iter().flatten().count() > 0);
            has_uplink_feedback = true;
          },
          CallServerMessage::Event { event: CallEvent::DownlinkMeasured { capacity_bps, .. }, .. } => {
            maybe_downlink = Some(capacity_bps);
          },
          other => panic!("unexpected {other:?}"),
        },
      }
      if has_uplink_feedback && let Some(capacity_bps) = maybe_downlink {
        return capacity_bps;
      }
    }
  })
  .await
  .expect("feedback and a downlink measurement within the deadline");
  assert!(measured > 0);
  drop(connection);

  // The same ticket again: refused.
  let (_again, mut send, mut recv) = open(address, hash).await;
  write_message(&mut send, &probe).await.unwrap();
  assert!(matches!(
    read_message::<CallServerMessage>(&mut recv, MAX_MESSAGE_BYTES)
      .await
      .unwrap(),
    CallServerMessage::Error { .. }
  ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn join_tokens_and_probe_tickets_dont_stand_in_for_each_other() {
  let (address, hash) = start_sfu();
  let (_probe, mut send, mut recv) = open(address, hash).await;
  let join_token_as_ticket = CallClientMessage::Probe {
    ticket: "dev:7:3:42".into(),
  };
  write_message(&mut send, &join_token_as_ticket).await.unwrap();
  assert!(matches!(
    read_message::<CallServerMessage>(&mut recv, MAX_MESSAGE_BYTES)
      .await
      .unwrap(),
    CallServerMessage::Error { .. }
  ));

  let (_join, mut send, mut recv) = open(address, hash).await;
  let ticket_as_join = CallClientMessage::Join {
    token: "dev-probe:3:42:t2".into(),
    capabilities: MediaCapabilities::default(),
    dave_transition_mode: Default::default(),
    maybe_dave_resume_epoch: None,
  };
  write_message(&mut send, &ticket_as_join).await.unwrap();
  assert!(matches!(
    read_message::<CallServerMessage>(&mut recv, MAX_MESSAGE_BYTES)
      .await
      .unwrap(),
    CallServerMessage::Error { .. }
  ));
}

async fn open(address: SocketAddr, hash: [u8; 32]) -> (Connection, SendStream, RecvStream) {
  let config = ClientConfig::builder()
    .with_bind_default()
    .with_server_certificate_hashes([hash.into()])
    .build();
  let connection = Endpoint::client(config)
    .unwrap()
    .connect(format!("https://{address}{CALL_PATH}"))
    .await
    .unwrap();
  let (send, recv) = connection.open_bi().await.unwrap().await.unwrap();
  (connection, send, recv)
}

fn start_sfu() -> (SocketAddr, [u8; 32]) {
  let config = SfuConfig {
    bind_addr: SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0)),
    ..SfuConfig::default()
  };
  let identity = Identity::self_signed(["localhost", "127.0.0.1", "::1"]).unwrap();
  let sfu = Sfu::bind(config, identity, Arc::new(DevTokenVerifier)).unwrap();
  let address = SocketAddr::new(Ipv6Addr::LOCALHOST.into(), sfu.local_addr().unwrap().port());
  let hash = *sfu.certificate_hash().as_ref();
  tokio::spawn(sfu.run());
  (address, hash)
}
