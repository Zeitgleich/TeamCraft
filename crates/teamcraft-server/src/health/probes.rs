//! The server's own probes of its dependencies (#1647, #1666): the object storage (one `HEAD` of a
//! key that doesn't exist, `[dependencies] storage_probe_secs`), the push gateway (its `/v1/info`)
//! and the SFU's public UDP port (a QUIC handshake, once over IPv4 and once over IPv6). So an
//! outage shows on an instance where nobody happens to upload, push or call.
//!
//! The probes run in a task of their own every `probe_interval_secs` (admin settings), each bounded
//! by `probe_timeout_secs`, and keep only their last reading: answered or not, and how long it took.
//! The health checks and the metrics read that; nothing on a request path waits for a probe.
//!
//! What a probe keeps and shows: the probe's name, whether it answered, milliseconds. No address,
//! no error text.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use time::OffsetDateTime;
use tracing::debug;
use url::Url;
use wtransport::error::{ConnectingError, ConnectionError};
use wtransport::{ClientConfig, Endpoint};

use crate::state::AppState;

/// Shortest interval between two rounds of probes, whatever the settings say.
const MIN_PROBE_INTERVAL: Duration = Duration::from_secs(10);
/// The path the UDP probe asks the SFU for: not the call path, so the SFU answers "not found" right
/// after the handshake and keeps nothing for it.
const SFU_PROBE_PATH: &str = "/health-probe";

/// One of the probes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Probe {
  ObjectStorage,
  PushGateway,
  MediaUdpIpv4,
  MediaUdpIpv6,
}

/// The last result of a probe.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ProbeReading {
  /// Not set up, probing is off, or no round has finished yet.
  #[default]
  NotConfigured,
  /// The server can't probe this from where it is (no IPv6 route, the name has no such address):
  /// that says nothing about the dependency.
  Unavailable,
  Answered {
    latency: Duration,
    at: OffsetDateTime,
  },
  Failed {
    at: OffsetDateTime,
  },
}

/// The last reading of every probe.
#[derive(Debug, Default)]
pub struct ProbeResults {
  readings: Mutex<[ProbeReading; Probe::ALL.len()]>,
}

impl Probe {
  pub const ALL: [Self; 4] = [
    Self::ObjectStorage,
    Self::PushGateway,
    Self::MediaUdpIpv4,
    Self::MediaUdpIpv6,
  ];

  /// The name in metrics.
  pub fn label(self) -> &'static str {
    match self {
      Self::ObjectStorage => "object_storage",
      Self::PushGateway => "push_gateway",
      Self::MediaUdpIpv4 => "media_udp_ipv4",
      Self::MediaUdpIpv6 => "media_udp_ipv6",
    }
  }

  fn index(self) -> usize {
    match self {
      Self::ObjectStorage => 0,
      Self::PushGateway => 1,
      Self::MediaUdpIpv4 => 2,
      Self::MediaUdpIpv6 => 3,
    }
  }
}

impl ProbeReading {
  /// What a probe that took `latency` and answered (or not) at `at` reads.
  pub fn of(is_answered: bool, latency: Duration, at: OffsetDateTime) -> Self {
    if is_answered {
      Self::Answered { latency, at }
    } else {
      Self::Failed { at }
    }
  }

  /// The milliseconds an answer took, for the check's number and the metrics.
  pub fn latency_millis(self) -> Option<i64> {
    match self {
      Self::Answered { latency, .. } => Some(i64::try_from(latency.as_millis()).unwrap_or(i64::MAX)),
      _ => None,
    }
  }
}

impl ProbeResults {
  pub fn get(&self, probe: Probe) -> ProbeReading {
    self.lock()[probe.index()]
  }

  pub fn set(&self, probe: Probe, reading: ProbeReading) {
    self.lock()[probe.index()] = reading;
  }

  fn lock(&self) -> MutexGuard<'_, [ProbeReading; Probe::ALL.len()]> {
    self.readings.lock().unwrap_or_else(PoisonError::into_inner)
  }
}

/// Runs the push gateway and SFU probes until the process ends. The interval and the timeout are
/// read again every round, so a change in the console applies from the next one.
pub fn spawn(state: AppState) {
  tokio::spawn(async move {
    let mut sfu = SfuProber::default();
    loop {
      let settings = state.settings.get().health_alerts.clone();
      let interval = Duration::from_secs(settings.probe_interval_secs).max(MIN_PROBE_INTERVAL);
      if settings.is_probing_enabled {
        let timeout = Duration::from_secs(settings.probe_timeout_secs.max(1));
        tokio::join!(probe_gateway(&state, timeout), sfu.probe(&state, timeout));
      } else {
        for probe in [Probe::PushGateway, Probe::MediaUdpIpv4, Probe::MediaUdpIpv6] {
          state.health.probes().set(probe, ProbeReading::NotConfigured);
        }
      }
      tokio::time::sleep(interval).await;
    }
  });
}

/// Asks the push gateway for its `/v1/info`, through its breaker: a gateway that is down and that
/// nobody pushes to is still found, and found back.
async fn probe_gateway(state: &AppState, timeout: Duration) {
  let Some(gateway) = state.push.gateway_client() else {
    state
      .health
      .probes()
      .set(Probe::PushGateway, ProbeReading::NotConfigured);
    return;
  };
  let started = Instant::now();
  let maybe_answered = gateway.probe(timeout).await;
  let reading = match maybe_answered {
    // The breaker is open and its next probe isn't due: it already says "down".
    None => return,
    Some(is_answered) => ProbeReading::of(is_answered, started.elapsed(), OffsetDateTime::now_utc()),
  };
  state.health.probes().set(Probe::PushGateway, reading);
}

/// The SFU's public UDP port from the outside, once per address family.
#[derive(Default)]
struct SfuProber {
  ipv4: FamilyProber,
  ipv6: FamilyProber,
}

/// The client endpoint of one family, kept while the SFU's address stays the same.
#[derive(Default)]
struct FamilyProber {
  maybe_endpoint: Option<(SocketAddr, Endpoint<wtransport::endpoint::endpoint_side::Client>)>,
}

impl SfuProber {
  async fn probe(&mut self, state: &AppState, timeout: Duration) {
    let probes = state.health.probes();
    let Some(target) = state
      .maybe_calls
      .as_ref()
      .and_then(|calls| SfuTarget::from_url(calls.sfu_url()))
    else {
      probes.set(Probe::MediaUdpIpv4, ProbeReading::NotConfigured);
      probes.set(Probe::MediaUdpIpv6, ProbeReading::NotConfigured);
      return;
    };
    // The lookup is bounded too: a resolver that hangs must not hold up the next rounds.
    let resolved = tokio::time::timeout(timeout, target.resolve())
      .await
      .unwrap_or_else(|_| Err(std::io::ErrorKind::TimedOut.into()));
    let (maybe_ipv4, maybe_ipv6) = match resolved {
      Ok(addresses) => addresses,
      Err(error) => {
        // The name doesn't resolve: from here the SFU can't be reached over either family.
        debug!(%error, "the SFU's name did not resolve for the UDP probe");
        let now = OffsetDateTime::now_utc();
        probes.set(Probe::MediaUdpIpv4, ProbeReading::Failed { at: now });
        probes.set(Probe::MediaUdpIpv6, ProbeReading::Failed { at: now });
        return;
      },
    };
    let (ipv4, ipv6) = tokio::join!(
      self.ipv4.probe(&target, maybe_ipv4, timeout),
      self.ipv6.probe(&target, maybe_ipv6, timeout),
    );
    probes.set(Probe::MediaUdpIpv4, ipv4);
    probes.set(Probe::MediaUdpIpv6, ipv6);
  }
}

impl FamilyProber {
  async fn probe(
    &mut self,
    target: &SfuTarget,
    maybe_address: Option<SocketAddr>,
    timeout: Duration,
  ) -> ProbeReading {
    let Some(address) = maybe_address else {
      // The SFU has no address of this family: nothing to probe.
      self.maybe_endpoint = None;
      return ProbeReading::NotConfigured;
    };
    if !has_route(address) {
      return ProbeReading::Unavailable;
    }
    let Some(endpoint) = self.endpoint_for(address) else {
      return ProbeReading::Unavailable;
    };
    let started = Instant::now();
    let is_answered = match tokio::time::timeout(timeout, endpoint.connect(target.probe_url.as_str())).await {
      Err(_) => false,
      Ok(result) => is_answer(&result),
    };
    ProbeReading::of(is_answered, started.elapsed(), OffsetDateTime::now_utc())
  }

  /// A client endpoint that resolves the SFU's name to `address`, so the handshake carries the
  /// right server name but goes to the family's address.
  fn endpoint_for(
    &mut self,
    address: SocketAddr,
  ) -> Option<&Endpoint<wtransport::endpoint::endpoint_side::Client>> {
    if self.maybe_endpoint.as_ref().is_none_or(|(at, _)| *at != address) {
      let bind: SocketAddr = match address {
        SocketAddr::V4(_) => (Ipv4Addr::UNSPECIFIED, 0).into(),
        SocketAddr::V6(_) => (Ipv6Addr::UNSPECIFIED, 0).into(),
      };
      let config = ClientConfig::builder()
        .with_bind_address(bind)
        .with_native_certs()
        .dns_resolver(FixedResolver(address))
        .build();
      match Endpoint::client(config) {
        Ok(endpoint) => self.maybe_endpoint = Some((address, endpoint)),
        Err(error) => {
          debug!(%error, "no client socket for the SFU's UDP probe");
          self.maybe_endpoint = None;
        },
      }
    }
    self.maybe_endpoint.as_ref().map(|(_, endpoint)| endpoint)
  }
}

/// Whether the SFU answered. Anything but silence is an answer: "not found" for the probe's path,
/// and also a refused certificate (a self-signed SFU), which still took a packet from the SFU
/// through the firewall. Silence (a timeout) is not.
fn is_answer(result: &Result<wtransport::Connection, ConnectingError>) -> bool {
  match result {
    Ok(_) | Err(ConnectingError::SessionRejected) => true,
    Err(ConnectingError::ConnectionError(error)) => {
      !matches!(error, ConnectionError::TimedOut | ConnectionError::LocallyClosed)
    },
    Err(_) => false,
  }
}

/// Whether this host has a route to `address`: a UDP socket's `connect` sends nothing, it only
/// picks the route. A server without IPv6 (a container network without it) can't probe over IPv6,
/// which says nothing about the SFU.
fn has_route(address: SocketAddr) -> bool {
  let bind: SocketAddr = match address {
    SocketAddr::V4(_) => (Ipv4Addr::UNSPECIFIED, 0).into(),
    SocketAddr::V6(_) => (Ipv6Addr::UNSPECIFIED, 0).into(),
  };
  UdpSocket::bind(bind)
    .and_then(|socket| socket.connect(address))
    .is_ok()
}

/// The SFU's public address, from `[calls] sfu_url`.
struct SfuTarget {
  host: String,
  port: u16,
  /// The SFU's URL with the probe's path.
  probe_url: String,
}

impl SfuTarget {
  fn from_url(sfu_url: &str) -> Option<Self> {
    let mut url = Url::parse(sfu_url).ok()?;
    let host = url
      .host_str()?
      .trim_start_matches('[')
      .trim_end_matches(']')
      .to_owned();
    let port = url.port_or_known_default()?;
    url.set_path(SFU_PROBE_PATH);
    url.set_query(None);
    Some(Self {
      host,
      port,
      probe_url: url.into(),
    })
  }

  /// The first IPv4 and the first IPv6 address of the SFU.
  async fn resolve(&self) -> std::io::Result<(Option<SocketAddr>, Option<SocketAddr>)> {
    if let Ok(ip) = self.host.parse::<IpAddr>() {
      let address = SocketAddr::new(ip, self.port);
      return Ok(match ip {
        IpAddr::V4(_) => (Some(address), None),
        IpAddr::V6(_) => (None, Some(address)),
      });
    }
    let addresses: Vec<SocketAddr> = tokio::net::lookup_host((self.host.as_str(), self.port))
      .await?
      .collect();
    Ok(first_of_each_family(&addresses))
  }
}

fn first_of_each_family(addresses: &[SocketAddr]) -> (Option<SocketAddr>, Option<SocketAddr>) {
  (
    addresses.iter().copied().find(SocketAddr::is_ipv4),
    addresses.iter().copied().find(SocketAddr::is_ipv6),
  )
}

/// Resolves every name to one address: the family's address of the SFU.
#[derive(Debug)]
struct FixedResolver(SocketAddr);

impl wtransport::config::DnsResolver for FixedResolver {
  fn resolve(&self, _host: &str) -> std::pin::Pin<Box<dyn wtransport::config::DnsLookupFuture>> {
    let address = self.0;
    Box::pin(async move { Ok(Some(address)) })
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_probe_asks_the_sfu_for_a_path_that_is_not_a_call() {
    let target = SfuTarget::from_url("https://sfu.example.com:3478/call?x=1").unwrap();
    assert_eq!((target.host.as_str(), target.port), ("sfu.example.com", 3478));
    assert_eq!(target.probe_url, "https://sfu.example.com:3478/health-probe");
    let literal = SfuTarget::from_url("https://[2001:db8::1]/call").unwrap();
    assert_eq!((literal.host.as_str(), literal.port), ("2001:db8::1", 443));
    assert!(SfuTarget::from_url("not a url").is_none());
  }

  #[tokio::test]
  async fn an_address_literal_is_probed_over_its_own_family_only() {
    let v6 = SfuTarget::from_url("https://[2001:db8::1]:3478/call").unwrap();
    let (maybe_v4, maybe_v6) = v6.resolve().await.unwrap();
    assert_eq!(maybe_v4, None);
    assert_eq!(maybe_v6, Some("[2001:db8::1]:3478".parse().unwrap()));
    let v4 = SfuTarget::from_url("https://192.0.2.7:3478/call").unwrap();
    assert_eq!(
      v4.resolve().await.unwrap(),
      (Some("192.0.2.7:3478".parse().unwrap()), None)
    );
  }

  #[test]
  fn each_family_gets_its_first_address() {
    let addresses: Vec<SocketAddr> = ["[2001:db8::1]:1", "192.0.2.1:1", "192.0.2.2:1", "[2001:db8::2]:1"]
      .iter()
      .map(|text| text.parse().unwrap())
      .collect();
    assert_eq!(
      first_of_each_family(&addresses),
      (Some(addresses[1]), Some(addresses[0]))
    );
  }

  #[test]
  fn silence_is_no_answer_and_a_refusal_is_one() {
    assert!(is_answer(&Err(ConnectingError::SessionRejected)));
    assert!(!is_answer(&Err(ConnectingError::ConnectionError(
      ConnectionError::TimedOut
    ))));
    assert!(!is_answer(&Err(ConnectingError::DnsNotFound)));
  }

  #[tokio::test]
  async fn an_sfu_that_answers_the_handshake_is_reachable_and_a_silent_port_is_not() {
    let identity = wtransport::Identity::self_signed(["localhost"]).unwrap();
    let config = wtransport::ServerConfig::builder()
      .with_bind_address((Ipv4Addr::LOCALHOST, 0).into())
      .with_identity(identity)
      .build();
    let server = Endpoint::server(config).unwrap();
    let address = server.local_addr().unwrap();
    tokio::spawn(async move {
      loop {
        let incoming = server.accept().await;
        if let Ok(request) = incoming.await {
          request.not_found().await;
        }
      }
    });
    let target = SfuTarget::from_url(&format!("https://localhost:{}/call", address.port())).unwrap();
    let mut prober = FamilyProber::default();
    let reading = prober
      .probe(&target, Some(address), Duration::from_secs(10))
      .await;
    assert!(matches!(reading, ProbeReading::Answered { .. }), "{reading:?}");

    // A port nobody listens on: no answer within the timeout.
    let silent = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let silent_address = silent.local_addr().unwrap();
    let reading = prober
      .probe(&target, Some(silent_address), Duration::from_millis(300))
      .await;
    assert!(matches!(reading, ProbeReading::Failed { .. }), "{reading:?}");
    drop(silent);
  }

  #[tokio::test]
  async fn a_family_the_sfu_has_no_address_in_is_not_probed() {
    let target = SfuTarget::from_url("https://sfu.example.com/call").unwrap();
    let reading = FamilyProber::default()
      .probe(&target, None, Duration::from_secs(1))
      .await;
    assert_eq!(reading, ProbeReading::NotConfigured);
  }

  #[test]
  fn readings_are_kept_per_probe() {
    let results = ProbeResults::default();
    let at = OffsetDateTime::UNIX_EPOCH;
    results.set(Probe::MediaUdpIpv6, ProbeReading::Failed { at });
    assert_eq!(results.get(Probe::MediaUdpIpv6), ProbeReading::Failed { at });
    assert_eq!(results.get(Probe::MediaUdpIpv4), ProbeReading::NotConfigured);
    let answered = ProbeReading::of(true, Duration::from_millis(12), at);
    assert_eq!(answered.latency_millis(), Some(12));
    assert_eq!(ProbeReading::Failed { at }.latency_millis(), None);
  }
}
