//! Port mapping for the direct path (#127, ARCHITECTURE.md §6.7, "Endpoint discovery"): asks the
//! router to forward a UDP port to the direct path socket, so the other device reaches it even
//! where the NAT would drop its packets. PCP (RFC 6887), NAT-PMP (RFC 6886) and UPnP-IGD are asked
//! at once; the first mapping granted is the candidate, renewed at half its lifetime while the call
//! runs and removed when it ends. Where no router answers, nothing changes: the call has its other
//! candidates and the SFU.
//!
//! IPv4 only: IPv6 needs no mapping (a firewall pinhole through PCP is a later step). Answers are
//! untrusted ([`crate::port_mapping_wire`]); UPnP's description and SOAP answers are read by
//! `igd-next`, which bounds their size and only follows URLs of the router that answered. A
//! router's external address that the other device couldn't reach is never offered. Addresses are
//! never logged.

use std::future::Future;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use futures_util::stream::FuturesUnordered;
use igd_next::aio::Gateway;
use igd_next::aio::tokio::{Tokio, search_gateway};
use igd_next::{PortMappingProtocol, SearchOptions};
use rand_core::{OsRng, RngCore};
use thiserror::Error;
use tokio::net::UdpSocket;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tracing::debug;

use crate::port_mapping_wire::{
  GATEWAY_PORT, GrantedMapping, MAX_ANSWER_BYTES, MappingAnswerError, PCP_NONCE_BYTES, is_reachable_external,
  nat_pmp_external_address_request, nat_pmp_map_request, pcp_map_request, read_nat_pmp_external_address,
  read_nat_pmp_map, read_pcp_map,
};

/// The lifetime asked for: renewed at half of it, so a mapping left behind by a crash is gone
/// within this.
pub const MAPPING_LIFETIME: Duration = Duration::from_secs(20 * 60);
/// NAT-PMP's and PCP's waits for an answer before the request is sent again (RFC 6886 §3.1 starts
/// at 250 ms and doubles); three tries, so a router that doesn't speak it costs under 2 s.
const ANSWER_WAITS: [Duration; 3] = [
  Duration::from_millis(250),
  Duration::from_millis(500),
  Duration::from_millis(1000),
];
/// How long UPnP's search for a router waits for answers.
const UPNP_SEARCH: Duration = Duration::from_secs(2);
/// The wait after a round that found no mapping; it doubles up to [`MAX_RETRY_WAIT`]. Routers
/// rarely start offering mappings during a call, so the tries are far apart.
const FIRST_RETRY_WAIT: Duration = Duration::from_secs(60);
const MAX_RETRY_WAIT: Duration = Duration::from_secs(15 * 60);
/// How long the removal of the mapping at the end may take.
const REMOVE_TIMEOUT: Duration = Duration::from_secs(2);
/// The description UPnP routers show in their mapping lists.
const UPNP_DESCRIPTION: &str = "TeamCraft call";

/// Keeps a port mapping for the direct path socket while it lives: maps when started, renews, and
/// removes the mapping once stopped or dropped (in a task of its own, so the removal runs after
/// the call is gone).
pub struct PortMapper {
  maybe_stop: Option<oneshot::Sender<()>>,
  task: JoinHandle<()>,
}

/// Which protocol granted a mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MappingProtocol {
  Pcp,
  NatPmp,
  Upnp,
}

/// A mapping the router holds for the direct path socket.
#[derive(Debug, Clone)]
pub struct PortMapping {
  pub protocol: MappingProtocol,
  /// Where the other device reaches the socket.
  pub external: SocketAddrV4,
  pub lifetime: Duration,
  network: LocalNetwork,
  internal_port: u16,
  nonce: [u8; PCP_NONCE_BYTES],
  maybe_upnp: Option<Arc<Gateway<Tokio>>>,
}

/// The local network as the mapping sees it: the default gateway, where PCP and NAT-PMP are asked,
/// and the own address toward it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalNetwork {
  pub gateway: SocketAddrV4,
  pub local_ip: Ipv4Addr,
}

/// Why no mapping was granted.
#[derive(Debug, Error)]
pub enum PortMapError {
  #[error("no IPv4 default gateway")]
  NoGateway,
  #[error(transparent)]
  Io(#[from] io::Error),
  #[error("the router didn't answer")]
  NoAnswer,
  #[error(transparent)]
  Answer(#[from] MappingAnswerError),
  #[error("UPnP: {0}")]
  Upnp(String),
}

type Attempt = Pin<Box<dyn Future<Output = Result<PortMapping, PortMapError>> + Send>>;

impl PortMapper {
  /// Starts mapping `internal_port`; `on_change` hears the external address whenever it changes
  /// (`None`: no mapping any more).
  pub fn start(internal_port: u16, on_change: impl Fn(Option<SocketAddrV4>) + Send + 'static) -> Self {
    let (stop, stopped) = oneshot::channel();
    let task = tokio::spawn(keep_mapped(internal_port, on_change, stopped));
    Self {
      maybe_stop: Some(stop),
      task,
    }
  }

  /// Stops renewing and removes the mapping (in the background).
  pub fn stop(&mut self) {
    if let Some(stop) = self.maybe_stop.take() {
      // The task may have ended already; nothing to remove then.
      let _ = stop.send(());
    }
  }

  /// Whether the background task still runs (it ends after the removal).
  pub fn is_running(&self) -> bool {
    !self.task.is_finished()
  }
}

impl Drop for PortMapper {
  fn drop(&mut self) {
    self.stop();
  }
}

impl LocalNetwork {
  /// The default IPv4 gateway and the own address on its interface, from the system's routes.
  pub async fn discover() -> Result<Self, PortMapError> {
    let interface = tokio::task::spawn_blocking(netdev::get_default_interface)
      .await
      .map_err(|_| PortMapError::NoGateway)?
      .map_err(|_| PortMapError::NoGateway)?;
    let gateway = interface
      .gateway
      .as_ref()
      .and_then(|gateway| gateway.ipv4.first().copied())
      .ok_or(PortMapError::NoGateway)?;
    let local_ip = interface
      .ipv4
      .iter()
      .map(|net| net.addr())
      .find(|ip| !ip.is_loopback() && !ip.is_unspecified())
      .ok_or(PortMapError::NoGateway)?;
    Ok(Self {
      gateway: SocketAddrV4::new(gateway, GATEWAY_PORT),
      local_ip,
    })
  }
}

/// Asks PCP, NAT-PMP and UPnP at once for a mapping of `internal_port`; the first granted one is
/// taken. A protocol that grants one later to another external address has it removed again; one
/// to the same external address is the same entry of the same router and stays with the winner.
pub async fn map_port(network: LocalNetwork, internal_port: u16) -> Result<PortMapping, PortMapError> {
  let mut nonce = [0u8; PCP_NONCE_BYTES];
  OsRng.fill_bytes(&mut nonce);
  let mut attempts: FuturesUnordered<Attempt> = FuturesUnordered::new();
  attempts.push(Box::pin(map_pcp(network, internal_port, nonce)));
  attempts.push(Box::pin(map_nat_pmp(network, internal_port)));
  attempts.push(Box::pin(map_upnp(network, internal_port)));
  let mut maybe_error = None;
  while let Some(result) = attempts.next().await {
    match result {
      Ok(mapping) => {
        let external = mapping.external;
        tokio::spawn(async move {
          while let Some(late) = attempts.next().await {
            if let Ok(late) = late
              && late.external != external
            {
              remove(&late).await;
            }
          }
        });
        return Ok(mapping);
      },
      Err(error) => maybe_error = Some(error),
    }
  }
  Err(maybe_error.unwrap_or(PortMapError::NoAnswer))
}

/// Renews `mapping` with the protocol that granted it; the router may move it.
pub async fn renew(mapping: &PortMapping) -> Result<PortMapping, PortMapError> {
  match mapping.protocol {
    MappingProtocol::Pcp => map_pcp(mapping.network, mapping.internal_port, mapping.nonce).await,
    MappingProtocol::NatPmp => map_nat_pmp(mapping.network, mapping.internal_port).await,
    MappingProtocol::Upnp => {
      let Some(gateway) = mapping.maybe_upnp.clone() else {
        return Err(PortMapError::NoAnswer);
      };
      upnp_add(
        gateway,
        mapping.network,
        mapping.internal_port,
        Some(mapping.external),
      )
      .await
    },
  }
}

/// Removes `mapping`; a router that doesn't answer lets it expire.
pub async fn remove(mapping: &PortMapping) {
  let result = match mapping.protocol {
    MappingProtocol::Pcp => {
      let request = pcp_map_request(
        mapping.network.local_ip,
        mapping.internal_port,
        Duration::ZERO,
        &mapping.nonce,
      );
      // PCP answers a removal with lifetime 0, which `read_pcp_map` refuses as too short: any
      // answer will do.
      exchange(mapping.network.gateway, &request, |_| Ok(())).await
    },
    MappingProtocol::NatPmp => {
      let request = nat_pmp_map_request(mapping.internal_port, Duration::ZERO);
      exchange(mapping.network.gateway, &request, |_| Ok(())).await
    },
    MappingProtocol::Upnp => match &mapping.maybe_upnp {
      Some(gateway) => gateway
        .remove_port(PortMappingProtocol::UDP, mapping.external.port())
        .await
        .map_err(|error| PortMapError::Upnp(error.to_string())),
      None => Ok(()),
    },
  };
  if let Err(error) = result {
    debug!(%error, protocol = ?mapping.protocol, "the port mapping couldn't be removed: it expires");
  }
}

/// Maps, renews at half the lifetime, retries with a backoff after failures, and removes the
/// mapping once stopped.
async fn keep_mapped(
  internal_port: u16,
  on_change: impl Fn(Option<SocketAddrV4>) + Send + 'static,
  mut stopped: oneshot::Receiver<()>,
) {
  let mut maybe_mapping: Option<PortMapping> = None;
  let mut failed: u32 = 0;
  let mut next = Instant::now();
  loop {
    tokio::select! {
      _ = &mut stopped => break,
      () = tokio::time::sleep_until(next) => {},
    }
    let result = match &maybe_mapping {
      Some(mapping) => renew(mapping).await,
      None => match LocalNetwork::discover().await {
        Ok(network) => map_port(network, internal_port).await,
        Err(error) => Err(error),
      },
    };
    match result {
      Ok(mapping) => {
        let is_new = maybe_mapping
          .as_ref()
          .is_none_or(|held| held.external != mapping.external);
        if is_new {
          debug!(protocol = ?mapping.protocol, lifetime_s = mapping.lifetime.as_secs(), "port mapped");
          on_change(Some(mapping.external));
        }
        failed = 0;
        next = Instant::now() + mapping.lifetime / 2;
        maybe_mapping = Some(mapping);
      },
      Err(error) => {
        debug!(%error, "no port mapping");
        if maybe_mapping.take().is_some() {
          on_change(None);
        }
        next = Instant::now() + retry_wait(failed);
        failed = failed.saturating_add(1);
      },
    }
  }
  if let Some(mapping) = maybe_mapping {
    // The router lets an unremoved mapping expire.
    let _ = tokio::time::timeout(REMOVE_TIMEOUT, remove(&mapping)).await;
  }
}

async fn map_pcp(
  network: LocalNetwork,
  internal_port: u16,
  nonce: [u8; PCP_NONCE_BYTES],
) -> Result<PortMapping, PortMapError> {
  let request = pcp_map_request(network.local_ip, internal_port, MAPPING_LIFETIME, &nonce);
  let granted = exchange(network.gateway, &request, |answer| {
    read_pcp_map(answer, internal_port, &nonce)
  })
  .await?;
  let ip = granted.maybe_external_ip.ok_or(PortMapError::NoAnswer)?;
  Ok(mapping(
    MappingProtocol::Pcp,
    network,
    internal_port,
    ip,
    granted,
    nonce,
  ))
}

async fn map_nat_pmp(network: LocalNetwork, internal_port: u16) -> Result<PortMapping, PortMapError> {
  let external_ip = exchange(
    network.gateway,
    &nat_pmp_external_address_request(),
    read_nat_pmp_external_address,
  )
  .await?;
  let request = nat_pmp_map_request(internal_port, MAPPING_LIFETIME);
  let granted = exchange(network.gateway, &request, |answer| {
    read_nat_pmp_map(answer, internal_port)
  })
  .await?;
  Ok(mapping(
    MappingProtocol::NatPmp,
    network,
    internal_port,
    external_ip,
    granted,
    [0; PCP_NONCE_BYTES],
  ))
}

async fn map_upnp(network: LocalNetwork, internal_port: u16) -> Result<PortMapping, PortMapError> {
  let mut options = SearchOptions::default();
  options.bind_addr = SocketAddr::new(IpAddr::V4(network.local_ip), 0);
  options.timeout = Some(UPNP_SEARCH);
  options.single_search_timeout = Some(UPNP_SEARCH);
  let gateway = search_gateway(options)
    .await
    .map_err(|error| PortMapError::Upnp(error.to_string()))?;
  upnp_add(Arc::new(gateway), network, internal_port, None).await
}

/// Adds (or renews) the UDP mapping: the same external port as the internal one where it is free,
/// else whichever the router picks.
async fn upnp_add(
  gateway: Arc<Gateway<Tokio>>,
  network: LocalNetwork,
  internal_port: u16,
  maybe_held: Option<SocketAddrV4>,
) -> Result<PortMapping, PortMapError> {
  let upnp_error = |error: &dyn std::fmt::Display| PortMapError::Upnp(error.to_string());
  let external_ip = match gateway
    .get_external_ip()
    .await
    .map_err(|error| upnp_error(&error))?
  {
    IpAddr::V4(ip) if is_reachable_external(ip) => ip,
    _ => return Err(MappingAnswerError::UnusableExternal.into()),
  };
  let local = SocketAddr::new(IpAddr::V4(network.local_ip), internal_port);
  let lease = u32::try_from(MAPPING_LIFETIME.as_secs()).unwrap_or(u32::MAX);
  let wanted = maybe_held.map_or(internal_port, |held| held.port());
  let external_port = match gateway
    .add_port(PortMappingProtocol::UDP, wanted, local, lease, UPNP_DESCRIPTION)
    .await
  {
    Ok(()) => wanted,
    Err(_) => gateway
      .add_any_port(PortMappingProtocol::UDP, local, lease, UPNP_DESCRIPTION)
      .await
      .map_err(|error| upnp_error(&error))?,
  };
  if external_port == 0 {
    return Err(MappingAnswerError::UnusableExternal.into());
  }
  Ok(PortMapping {
    protocol: MappingProtocol::Upnp,
    external: SocketAddrV4::new(external_ip, external_port),
    lifetime: MAPPING_LIFETIME,
    network,
    internal_port,
    nonce: [0; PCP_NONCE_BYTES],
    maybe_upnp: Some(gateway),
  })
}

/// Sends `request` to the gateway and waits for an answer `read` takes, sending again after each
/// of [`ANSWER_WAITS`]. A refusal or a useless mapping ends it at once; anything else that arrives
/// and isn't the answer (another request's answer, garbage) is skipped.
async fn exchange<T>(
  gateway: SocketAddrV4,
  request: &[u8],
  read: impl Fn(&[u8]) -> Result<T, MappingAnswerError>,
) -> Result<T, PortMapError> {
  let socket = UdpSocket::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0))).await?;
  // Connected: the system drops datagrams from anybody but the gateway's port.
  socket.connect(SocketAddr::V4(gateway)).await?;
  let mut buffer = vec![0u8; MAX_ANSWER_BYTES + 1];
  for wait in ANSWER_WAITS {
    socket.send(request).await?;
    let deadline = Instant::now() + wait;
    loop {
      let received = tokio::time::timeout_at(deadline, socket.recv(&mut buffer)).await;
      let Ok(received) = received else {
        break;
      };
      let length = match received {
        Ok(length) => length,
        // ICMP port unreachable: nobody listens there.
        Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => {
          return Err(PortMapError::NoAnswer);
        },
        Err(error) => return Err(error.into()),
      };
      match read(&buffer[..length]) {
        Ok(answer) => return Ok(answer),
        // The router's word on this request: no need to ask again.
        Err(
          final_answer @ (MappingAnswerError::Refused(_)
          | MappingAnswerError::UnusableExternal
          | MappingAnswerError::TooShort(_)),
        ) => return Err(final_answer.into()),
        Err(error) => debug!(%error, "an answer that isn't the router's to this request"),
      }
    }
  }
  Err(PortMapError::NoAnswer)
}

fn mapping(
  protocol: MappingProtocol,
  network: LocalNetwork,
  internal_port: u16,
  external_ip: Ipv4Addr,
  granted: GrantedMapping,
  nonce: [u8; PCP_NONCE_BYTES],
) -> PortMapping {
  PortMapping {
    protocol,
    external: SocketAddrV4::new(external_ip, granted.external_port),
    lifetime: granted.lifetime,
    network,
    internal_port,
    nonce,
    maybe_upnp: None,
  }
}

fn retry_wait(failed: u32) -> Duration {
  FIRST_RETRY_WAIT
    .saturating_mul(1u32 << failed.min(16))
    .min(MAX_RETRY_WAIT)
}

#[cfg(test)]
mod tests {
  use std::sync::Mutex;

  use super::*;

  const INTERNAL_PORT: u16 = 40_000;
  const PUBLIC: Ipv4Addr = Ipv4Addr::new(198, 51, 100, 7);

  /// A NAT-PMP router on loopback: the mapping is read from its answers.
  #[tokio::test]
  async fn nat_pmp_maps_through_a_router_that_answers() {
    let router = FakeRouter::start(|request| match request {
      [0, 0] => Some(external_answer(PUBLIC).to_vec()),
      [0, 1, ..] => Some(map_answer(INTERNAL_PORT, 41_000, 1200).to_vec()),
      _ => None,
    })
    .await;
    let mapping = map_nat_pmp(router.network(), INTERNAL_PORT).await.unwrap();
    assert_eq!(mapping.protocol, MappingProtocol::NatPmp);
    assert_eq!(mapping.external, SocketAddrV4::new(PUBLIC, 41_000));
    assert_eq!(mapping.lifetime, Duration::from_secs(1200));
    remove(&mapping).await;
    let requests = router.requests();
    assert_eq!(
      requests.last().unwrap()[..],
      nat_pmp_map_request(INTERNAL_PORT, Duration::ZERO),
      "the removal asks for no time"
    );
  }

  /// Garbage and other requests' answers before the real one are skipped; it still maps.
  #[tokio::test]
  async fn garbage_before_the_answer_is_skipped() {
    let router = FakeRouter::start(|request| match request {
      [0, 0] => Some(external_answer(PUBLIC).to_vec()),
      [0, 1, ..] => Some(map_answer(INTERNAL_PORT, 41_000, 1200).to_vec()),
      _ => None,
    })
    .with_noise(vec![
      vec![0xff; 3],
      vec![0, 129],
      map_answer(INTERNAL_PORT + 1, 9, 1200).to_vec(),
    ])
    .await;
    let mapping = map_nat_pmp(router.network(), INTERNAL_PORT).await.unwrap();
    assert_eq!(mapping.external.port(), 41_000);
  }

  /// A router that refuses says so at once.
  #[tokio::test]
  async fn a_refusal_ends_the_attempt() {
    let router = FakeRouter::start(|request| match request {
      [0, 0] => Some(external_answer(PUBLIC).to_vec()),
      [0, 1, ..] => {
        let mut refused = map_answer(INTERNAL_PORT, 0, 0);
        refused[3] = 2;
        Some(refused.to_vec())
      },
      _ => None,
    })
    .await;
    let error = map_nat_pmp(router.network(), INTERNAL_PORT).await.unwrap_err();
    assert!(matches!(
      error,
      PortMapError::Answer(MappingAnswerError::Refused(2))
    ));
  }

  /// A router behind another NAT names an address of the outer NAT's inside: no candidate.
  #[tokio::test]
  async fn an_external_address_behind_another_nat_is_not_taken() {
    let router = FakeRouter::start(|request| match request {
      [0, 0] => Some(external_answer(Ipv4Addr::new(100, 64, 3, 4)).to_vec()),
      _ => None,
    })
    .await;
    let error = map_nat_pmp(router.network(), INTERNAL_PORT).await.unwrap_err();
    assert!(
      matches!(error, PortMapError::Answer(MappingAnswerError::UnusableExternal)),
      "{error}"
    );
  }

  /// PCP's answer names the address and port; its renewal carries the same nonce.
  #[tokio::test]
  async fn pcp_maps_and_renews_with_the_same_nonce() {
    let router = FakeRouter::start(|request| {
      (request.len() == 60 && request[0] == 2).then(|| {
        let mut answer = [0u8; 60];
        answer[0] = 2;
        answer[1] = 0x81;
        answer[4..8].copy_from_slice(&1200u32.to_be_bytes());
        answer[24..36].copy_from_slice(&request[24..36]);
        answer[36] = 17;
        answer[40..42].copy_from_slice(&request[40..42]);
        answer[42..44].copy_from_slice(&41_000u16.to_be_bytes());
        answer[44..60].copy_from_slice(&PUBLIC.to_ipv6_mapped().octets());
        answer.to_vec()
      })
    })
    .await;
    let mapping = map_pcp(router.network(), INTERNAL_PORT, [5; 12]).await.unwrap();
    assert_eq!(mapping.external, SocketAddrV4::new(PUBLIC, 41_000));
    let renewed = renew(&mapping).await.unwrap();
    assert_eq!(renewed.external, mapping.external);
    let requests = router.requests();
    assert_eq!(requests[0][24..36], requests[1][24..36]);
  }

  /// Nobody listens: the attempt ends within the waits, without a mapping.
  #[tokio::test]
  async fn a_router_that_never_answers_costs_under_two_seconds() {
    let silent = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let SocketAddr::V4(gateway) = silent.local_addr().unwrap() else {
      unreachable!()
    };
    let network = LocalNetwork {
      gateway,
      local_ip: Ipv4Addr::LOCALHOST,
    };
    let started = Instant::now();
    assert!(map_nat_pmp(network, INTERNAL_PORT).await.is_err());
    assert!(started.elapsed() < Duration::from_secs(2));
  }

  #[test]
  fn retries_back_off_up_to_fifteen_minutes() {
    assert_eq!(retry_wait(0), Duration::from_secs(60));
    assert_eq!(retry_wait(1), Duration::from_secs(120));
    assert_eq!(retry_wait(10), Duration::from_secs(900));
  }

  type Answer = Box<dyn Fn(&[u8]) -> Option<Vec<u8>> + Send + Sync>;

  /// Answers requests on loopback as a router would, and records them.
  struct FakeRouter {
    address: SocketAddrV4,
    requests: Arc<Mutex<Vec<Vec<u8>>>>,
  }

  struct FakeRouterBuilder {
    answer: Answer,
    noise: Vec<Vec<u8>>,
  }

  impl FakeRouter {
    fn start(answer: impl Fn(&[u8]) -> Option<Vec<u8>> + Send + Sync + 'static) -> FakeRouterBuilder {
      FakeRouterBuilder {
        answer: Box::new(answer),
        noise: Vec::new(),
      }
    }

    fn network(&self) -> LocalNetwork {
      LocalNetwork {
        gateway: self.address,
        local_ip: Ipv4Addr::LOCALHOST,
      }
    }

    fn requests(&self) -> Vec<Vec<u8>> {
      self.requests.lock().unwrap().clone()
    }
  }

  impl FakeRouterBuilder {
    fn with_noise(mut self, noise: Vec<Vec<u8>>) -> Self {
      self.noise = noise;
      self
    }
  }

  impl std::future::IntoFuture for FakeRouterBuilder {
    type Output = FakeRouter;
    type IntoFuture = Pin<Box<dyn Future<Output = FakeRouter> + Send>>;

    fn into_future(self) -> Self::IntoFuture {
      Box::pin(async move {
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let SocketAddr::V4(address) = socket.local_addr().unwrap() else {
          unreachable!()
        };
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&requests);
        tokio::spawn(async move {
          let mut buffer = [0u8; 1500];
          loop {
            let (length, from) = socket.recv_from(&mut buffer).await.unwrap();
            let request = buffer[..length].to_vec();
            recorded.lock().unwrap().push(request.clone());
            for noise in &self.noise {
              socket.send_to(noise, from).await.unwrap();
            }
            if let Some(answer) = (self.answer)(&request) {
              socket.send_to(&answer, from).await.unwrap();
            }
          }
        });
        FakeRouter { address, requests }
      })
    }
  }

  fn map_answer(internal: u16, external: u16, lifetime: u32) -> [u8; 16] {
    let mut answer = [0u8; 16];
    answer[1] = 129;
    answer[8..10].copy_from_slice(&internal.to_be_bytes());
    answer[10..12].copy_from_slice(&external.to_be_bytes());
    answer[12..16].copy_from_slice(&lifetime.to_be_bytes());
    answer
  }

  fn external_answer(ip: Ipv4Addr) -> [u8; 12] {
    let mut answer = [0u8; 12];
    answer[1] = 128;
    answer[8..12].copy_from_slice(&ip.octets());
    answer
  }
}
