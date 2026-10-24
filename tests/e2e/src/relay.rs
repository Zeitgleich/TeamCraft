//! A TCP relay between the server and the dev Postgres that a test can cut and restore, to make the
//! database "briefly unavailable" for one server without touching the Postgres the other tests share.

use std::net::SocketAddr;
use std::sync::Mutex;

use tokio::io::copy_bidirectional;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;

/// Forwards `127.0.0.1:<port>` to a target address until dropped.
pub struct TcpRelay {
  pub port: u16,
  target: String,
  /// Bumped on every cut: open connections watch it and close.
  cuts: watch::Sender<u64>,
  /// The accept loop while the relay is up; `None` while it is cut (the port refuses connections).
  accepting: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl TcpRelay {
  pub async fn start(target: &str) -> std::io::Result<Self> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let (cuts, _) = watch::channel(0_u64);
    let relay = Self {
      port,
      target: target.to_owned(),
      cuts,
      accepting: Mutex::new(None),
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
    let task = tokio::spawn(accept_loop(listener, self.target.clone(), self.cuts.clone()));
    *self.accepting.lock().expect("relay state") = Some(task);
    Ok(())
  }
}

impl Drop for TcpRelay {
  fn drop(&mut self) {
    self.cut();
  }
}

async fn accept_loop(listener: TcpListener, target: String, cuts: watch::Sender<u64>) {
  while let Ok((mut inbound, _)) = listener.accept().await {
    let target = target.clone();
    let mut cut = cuts.subscribe();
    tokio::spawn(async move {
      let Ok(mut outbound) = TcpStream::connect(&target).await else {
        return;
      };
      tokio::select! {
        _ = copy_bidirectional(&mut inbound, &mut outbound) => {},
        _ = cut.changed() => {},
      }
    });
  }
}
