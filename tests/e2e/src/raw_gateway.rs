//! A gateway connection without the client core: the test decides when to identify, which sequence
//! number to resume from and whether to read at all (a slow consumer).

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use teamcraft_proto::{DeviceActivity, DeviceId, GatewayClientMessage, GatewayServerMessage};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::stack::Stack;

const FRAME_TIMEOUT: Duration = Duration::from_secs(15);

pub struct RawGateway {
  socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
}

/// How the connection ended, as far as the client can tell.
#[derive(Debug, PartialEq, Eq)]
pub enum Ending {
  /// A close frame with this code.
  Closed(u16),
  /// The TCP connection ended without a close frame.
  Dropped,
}

impl RawGateway {
  /// Opens the WebSocket and reads `hello`.
  pub async fn connect(stack: &Stack) -> Self {
    let url = format!("{}/api/v1/gateway", stack.base_url.replacen("http", "ws", 1));
    let (socket, _) = tokio_tungstenite::connect_async(url.as_str())
      .await
      .expect("gateway WebSocket connects");
    let mut gateway = Self { socket };
    match gateway.receive().await {
      Some(GatewayServerMessage::Hello { .. }) => gateway,
      other => panic!("expected hello, got {other:?}"),
    }
  }

  /// The client side's TCP port (to find the connection in the kernel's tables).
  pub fn local_port(&self) -> u16 {
    match self.socket.get_ref() {
      MaybeTlsStream::Plain(stream) => stream.local_addr().expect("local address").port(),
      _ => unreachable!("the test server speaks plain HTTP"),
    }
  }

  /// A new session; returns its ID.
  pub async fn identify(&mut self, token: String, device_id: DeviceId) -> String {
    self
      .send(&GatewayClientMessage::Identify {
        token,
        device_id,
        activity: DeviceActivity::Active,
      })
      .await;
    match self.receive().await {
      Some(GatewayServerMessage::Ready { session_id, .. }) => session_id,
      other => panic!("expected ready, got {other:?}"),
    }
  }

  /// Tries to resume; returns the server's answer (`Resumed` or `SessionInvalid`).
  pub async fn resume(
    &mut self,
    token: String,
    device_id: DeviceId,
    session_id: &str,
    last_seq: u64,
  ) -> Option<GatewayServerMessage> {
    self
      .send(&GatewayClientMessage::Resume {
        token,
        session_id: session_id.to_owned(),
        last_seq,
        device_id,
      })
      .await;
    self.receive().await
  }

  pub async fn send(&mut self, message: &GatewayClientMessage) {
    let text = serde_json::to_string(message).expect("client frame serializes");
    self
      .socket
      .send(Message::Text(text.into()))
      .await
      .expect("sending a gateway frame");
  }

  /// The next server frame; `None` when the connection ended or nothing came within 15 s.
  pub async fn receive(&mut self) -> Option<GatewayServerMessage> {
    loop {
      match tokio::time::timeout(FRAME_TIMEOUT, self.socket.next()).await {
        Ok(Some(Ok(Message::Text(text)))) => {
          return Some(serde_json::from_str(text.as_str()).expect("server frame parses"));
        },
        Ok(Some(Ok(Message::Close(_)) | Err(_)) | None) | Err(_) => return None,
        Ok(Some(Ok(_))) => {},
      }
    }
  }

  /// Reads everything that is queued until the connection ends, within `timeout`. Returns the
  /// number of event frames and how the connection ended (`None`: still open at the deadline).
  pub async fn drain(&mut self, timeout: Duration) -> (usize, Option<Ending>) {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut events = 0;
    loop {
      match tokio::time::timeout_at(deadline, self.socket.next()).await {
        Err(_) => return (events, None),
        Ok(None | Some(Err(_))) => return (events, Some(Ending::Dropped)),
        Ok(Some(Ok(Message::Close(frame)))) => {
          let code = frame.map_or(1005, |frame| u16::from(frame.code));
          return (events, Some(Ending::Closed(code)));
        },
        Ok(Some(Ok(Message::Text(text)))) => {
          if text.as_str().starts_with(r#"{"type":"event""#) {
            events += 1;
          }
        },
        Ok(Some(Ok(_))) => {},
      }
    }
  }
}
