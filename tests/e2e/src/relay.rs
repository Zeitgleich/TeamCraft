//! A TCP relay between the server and the dev Postgres that a test can cut and restore, to make the
//! database "briefly unavailable" for one server without touching the Postgres the other tests share.

use std::net::SocketAddr;
use std::sync::Mutex;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt, copy_bidirectional};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;

/// Forwards `127.0.0.1:<port>` to a target address until dropped.
pub struct TcpRelay {
  pub port: u16,
  target: String,
  /// Bumped on every cut: open connections watch it and close.
  cuts: watch::Sender<u64>,
  /// The accept loop while the relay is up; `None` while it is cut (the port refuses connections).
  accepting: Mutex<Option<tokio::task::JoinHandle<()>>>,
  /// Added to every round trip through the relay.
  latency: Duration,
}

impl TcpRelay {
  pub async fn start(target: &str) -> std::io::Result<Self> {
    Self::start_with_latency(target, Duration::ZERO).await
  }

  /// Like [`Self::start`], with `latency` added to every round trip (a client far from the
  /// server, issue #714).
  pub async fn start_with_latency(target: &str, latency: Duration) -> std::io::Result<Self> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let (cuts, _) = watch::channel(0_u64);
    let relay = Self {
      port,
      target: target.to_owned(),
      cuts,
      accepting: Mutex::new(None),
      latency,
    };
    relay.accept_on(listener)?;
    Ok(relay)
  }

  /// Closes every open connection and stops listening, so new connections are refused (like a
  /// database that is restarting).
  pub fn cut(&self) {
    if let Some(task) = self.accepting.lock().expect("relay state").take() {
      task.abort();
    }
    self.cuts.send_modify(|cuts| *cuts += 1);
  }

  /// Listens on the same port again.
  pub fn restore(&self) {
    let listener = std::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], self.port)))
      .expect("binding the relay port again");
    self.accept_on(listener).expect("restoring the relay");
  }

  fn accept_on(&self, listener: std::net::TcpListener) -> std::io::Result<()> {
    listener.set_nonblocking(true)?;
    let listener = TcpListener::from_std(listener)?;
    let task = tokio::spawn(accept_loop(
      listener,
      self.target.clone(),
      self.cuts.clone(),
      self.latency,
    ));
    *self.accepting.lock().expect("relay state") = Some(task);
    Ok(())
  }
}

impl Drop for TcpRelay {
  fn drop(&mut self) {
    self.cut();
  }
}

async fn accept_loop(listener: TcpListener, target: String, cuts: watch::Sender<u64>, latency: Duration) {
  while let Ok((mut inbound, _)) = listener.accept().await {
    let target = target.clone();
    let mut cut = cuts.subscribe();
    tokio::spawn(async move {
      let Ok(mut outbound) = TcpStream::connect(&target).await else {
        return;
      };
      if latency.is_zero() {
        tokio::select! {
          _ = copy_bidirectional(&mut inbound, &mut outbound) => {},
          _ = cut.changed() => {},
        }
        return;
      }
      let (inbound_read, inbound_write) = inbound.into_split();
      let (outbound_read, outbound_write) = outbound.into_split();
      // Half the round trip each way, so a request and its answer take `latency` together.
      let one_way = latency / 2;
      tokio::select! {
        _ = delayed_copy(inbound_read, outbound_write, one_way) => {},
        _ = delayed_copy(outbound_read, inbound_write, one_way) => {},
        _ = cut.changed() => {},
      }
    });
  }
}

/// Copies `from` to `to`, each chunk `delay` after it arrived; chunks keep their order and don't
/// wait for each other, like bytes on a long link.
async fn delayed_copy(mut from: OwnedReadHalf, mut to: OwnedWriteHalf, delay: Duration) {
  let (chunks, mut queued) = mpsc::unbounded_channel::<(Instant, Vec<u8>)>();
  let reader = async move {
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
      match from.read(&mut buffer).await {
        Ok(0) | Err(_) => return,
        Ok(read) => {
          if chunks
            .send((Instant::now() + delay, buffer[..read].to_vec()))
            .is_err()
          {
            return;
          }
        },
      }
    }
  };
  let writer = async move {
    while let Some((due, chunk)) = queued.recv().await {
      tokio::time::sleep_until(due).await;
      if to.write_all(&chunk).await.is_err() {
        return;
      }
    }
    let _ = to.shutdown().await;
  };
  tokio::join!(reader, writer);
}
