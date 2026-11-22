//! The router protocols that map a UDP port for the direct path (#127, ARCHITECTURE.md §6.7,
//! "Endpoint discovery"), as bytes: NAT-PMP (RFC 6886) and PCP (RFC 6887), requests built and
//! answers read, without I/O. `crate::port_mapping` sends them.
//!
//! A router's answer is untrusted input: anything on the local network can answer from the
//! gateway's address. An answer is taken only when it is exactly the answer to the request (version,
//! opcode, protocol, internal port, PCP's nonce), says success, and names an external address that
//! the other device could reach (a global IPv4 address, a port that isn't 0); its lifetime is
//! clamped. Everything else is an error, never a panic.

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddrV4};
use std::time::Duration;

use thiserror::Error;

/// The port NAT-PMP and PCP servers listen on, at the default gateway (RFC 6886 §3.1, RFC 6887 §19.1).
pub const GATEWAY_PORT: u16 = 5351;
/// The largest answer read; a PCP message is at most 1100 bytes (RFC 6887 §7).
pub const MAX_ANSWER_BYTES: usize = 1100;
/// A mapping the router grants for less than this is useless for a call: it is refused.
pub const MIN_LIFETIME: Duration = Duration::from_secs(30);
/// A longer lifetime a router grants is taken as this: the mapping is renewed at half of it anyway.
pub const MAX_LIFETIME: Duration = Duration::from_secs(24 * 60 * 60);
/// The length of a PCP nonce (RFC 6887 §11.1).
pub const PCP_NONCE_BYTES: usize = 12;

const NAT_PMP_VERSION: u8 = 0;
const NAT_PMP_OP_EXTERNAL_ADDRESS: u8 = 0;
const NAT_PMP_OP_MAP_UDP: u8 = 1;
const NAT_PMP_ANSWER_BIT: u8 = 128;
const NAT_PMP_EXTERNAL_ANSWER_LEN: usize = 12;
const NAT_PMP_MAP_ANSWER_LEN: usize = 16;
const PCP_VERSION: u8 = 2;
const PCP_OP_MAP: u8 = 1;
const PCP_ANSWER_BIT: u8 = 0x80;
const PCP_MAP_LEN: usize = 60;
const UDP_PROTOCOL: u8 = 17;

/// A mapping a router granted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GrantedMapping {
  /// The external port, and for PCP the external address.
  pub external_port: u16,
  pub maybe_external_ip: Option<Ipv4Addr>,
  /// Clamped to [`MAX_LIFETIME`].
  pub lifetime: Duration,
}

/// Why a router's answer isn't taken.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum MappingAnswerError {
  #[error("the answer has {0} bytes")]
  Length(usize),
  #[error("the answer is of version {0}")]
  Version(u8),
  #[error("the answer is to another request (opcode {0})")]
  Opcode(u8),
  #[error("the router refused: result code {0}")]
  Refused(u16),
  #[error("the answer is to another mapping")]
  OtherMapping,
  #[error("the external address can't be reached from outside")]
  UnusableExternal,
  #[error("the mapping lasts too short: {0:?}")]
  TooShort(Duration),
}

/// NAT-PMP's request for the external address (RFC 6886 §3.2).
pub fn nat_pmp_external_address_request() -> [u8; 2] {
  [NAT_PMP_VERSION, NAT_PMP_OP_EXTERNAL_ADDRESS]
}

/// NAT-PMP's request to map the UDP `internal_port`, preferably to the same external port; a zero
/// `lifetime` removes it (RFC 6886 §3.3, §3.4).
pub fn nat_pmp_map_request(internal_port: u16, lifetime: Duration) -> [u8; 12] {
  let suggested = if lifetime.is_zero() { 0 } else { internal_port };
  let mut request = [0u8; 12];
  request[0] = NAT_PMP_VERSION;
  request[1] = NAT_PMP_OP_MAP_UDP;
  request[4..6].copy_from_slice(&internal_port.to_be_bytes());
  request[6..8].copy_from_slice(&suggested.to_be_bytes());
  request[8..12].copy_from_slice(&lifetime_secs(lifetime).to_be_bytes());
  request
}

/// Reads NAT-PMP's answer with the external address.
pub fn read_nat_pmp_external_address(answer: &[u8]) -> Result<Ipv4Addr, MappingAnswerError> {
  let fields = nat_pmp_header(answer, NAT_PMP_OP_EXTERNAL_ADDRESS, NAT_PMP_EXTERNAL_ANSWER_LEN)?;
  let ip = Ipv4Addr::new(fields[8], fields[9], fields[10], fields[11]);
  if !is_reachable_external(ip) {
    return Err(MappingAnswerError::UnusableExternal);
  }
  Ok(ip)
}

/// Reads NAT-PMP's answer to [`nat_pmp_map_request`] for `internal_port`.
pub fn read_nat_pmp_map(answer: &[u8], internal_port: u16) -> Result<GrantedMapping, MappingAnswerError> {
  let fields = nat_pmp_header(answer, NAT_PMP_OP_MAP_UDP, NAT_PMP_MAP_ANSWER_LEN)?;
  if be_u16(&fields[8..10]) != internal_port {
    return Err(MappingAnswerError::OtherMapping);
  }
  let external_port = be_u16(&fields[10..12]);
  let lifetime = Duration::from_secs(u64::from(be_u32(&fields[12..16])));
  granted(external_port, None, lifetime)
}

/// PCP's MAP request for the UDP `internal_port` of `client_ip`, preferably to the same external
/// port; a zero `lifetime` removes it (RFC 6887 §7.1, §11.1). The nonce ties the answer to it.
pub fn pcp_map_request(
  client_ip: Ipv4Addr,
  internal_port: u16,
  lifetime: Duration,
  nonce: &[u8; PCP_NONCE_BYTES],
) -> [u8; PCP_MAP_LEN] {
  let suggested = if lifetime.is_zero() { 0 } else { internal_port };
  let mut request = [0u8; PCP_MAP_LEN];
  request[0] = PCP_VERSION;
  request[1] = PCP_OP_MAP;
  request[4..8].copy_from_slice(&lifetime_secs(lifetime).to_be_bytes());
  request[8..24].copy_from_slice(&client_ip.to_ipv6_mapped().octets());
  request[24..36].copy_from_slice(nonce);
  request[36] = UDP_PROTOCOL;
  request[40..42].copy_from_slice(&internal_port.to_be_bytes());
  request[42..44].copy_from_slice(&suggested.to_be_bytes());
  // No suggested external address: the IPv4-mapped unspecified address (RFC 6887 §11.1).
  request[44..60].copy_from_slice(&Ipv4Addr::UNSPECIFIED.to_ipv6_mapped().octets());
  request
}

/// Reads PCP's answer to [`pcp_map_request`]. Options after the MAP fields are ignored.
pub fn read_pcp_map(
  answer: &[u8],
  internal_port: u16,
  nonce: &[u8; PCP_NONCE_BYTES],
) -> Result<GrantedMapping, MappingAnswerError> {
  if answer.len() < PCP_MAP_LEN || answer.len() > MAX_ANSWER_BYTES || !answer.len().is_multiple_of(4) {
    return Err(MappingAnswerError::Length(answer.len()));
  }
  if answer[0] != PCP_VERSION {
    return Err(MappingAnswerError::Version(answer[0]));
  }
  if answer[1] != PCP_ANSWER_BIT | PCP_OP_MAP {
    return Err(MappingAnswerError::Opcode(answer[1]));
  }
  let result = answer[3];
  if result != 0 {
    return Err(MappingAnswerError::Refused(u16::from(result)));
  }
  let is_ours =
    answer[24..36] == nonce[..] && answer[36] == UDP_PROTOCOL && be_u16(&answer[40..42]) == internal_port;
  if !is_ours {
    return Err(MappingAnswerError::OtherMapping);
  }
  let lifetime = Duration::from_secs(u64::from(be_u32(&answer[4..8])));
  let external_port = be_u16(&answer[42..44]);
  let mut external = [0u8; 16];
  external.copy_from_slice(&answer[44..60]);
  let external_ip = Ipv6Addr::from(external)
    .to_ipv4_mapped()
    .ok_or(MappingAnswerError::UnusableExternal)?;
  granted(external_port, Some(external_ip), lifetime)
}

/// The external address of a mapping, once the router said both its address and its port.
pub fn external_address(ip: Ipv4Addr, port: u16) -> SocketAddrV4 {
  SocketAddrV4::new(ip, port)
}

/// An external IPv4 address another device can reach: not private, shared (carrier-grade NAT,
/// RFC 6598), loopback, link-local, multicast, broadcast, unspecified or reserved. A router behind
/// another NAT answers with an address of the outer NAT's inside, which is useless as a candidate.
pub fn is_reachable_external(ip: Ipv4Addr) -> bool {
  let [first, second, ..] = ip.octets();
  let is_shared = first == 100 && (64..128).contains(&second);
  let is_reserved = first == 0 || first >= 240;
  !(ip.is_private()
    || is_shared
    || is_reserved
    || ip.is_loopback()
    || ip.is_link_local()
    || ip.is_multicast()
    || ip.is_broadcast()
    || ip.is_unspecified())
}

/// Checks a NAT-PMP answer's length, version, opcode and result; returns it whole.
fn nat_pmp_header(answer: &[u8], opcode: u8, len: usize) -> Result<&[u8], MappingAnswerError> {
  // Version and opcode first: a PCP server answers a NAT-PMP request with its own, shorter error.
  let (Some(&version), Some(&answer_opcode)) = (answer.first(), answer.get(1)) else {
    return Err(MappingAnswerError::Length(answer.len()));
  };
  if version != NAT_PMP_VERSION {
    return Err(MappingAnswerError::Version(version));
  }
  if answer_opcode != NAT_PMP_ANSWER_BIT | opcode {
    return Err(MappingAnswerError::Opcode(answer_opcode));
  }
  if answer.len() < 4 {
    return Err(MappingAnswerError::Length(answer.len()));
  }
  let result = be_u16(&answer[2..4]);
  if result != 0 {
    return Err(MappingAnswerError::Refused(result));
  }
  if answer.len() != len {
    return Err(MappingAnswerError::Length(answer.len()));
  }
  Ok(answer)
}

fn granted(
  external_port: u16,
  maybe_external_ip: Option<Ipv4Addr>,
  lifetime: Duration,
) -> Result<GrantedMapping, MappingAnswerError> {
  if external_port == 0 {
    return Err(MappingAnswerError::UnusableExternal);
  }
  if maybe_external_ip.is_some_and(|ip| !is_reachable_external(ip)) {
    return Err(MappingAnswerError::UnusableExternal);
  }
  if lifetime < MIN_LIFETIME {
    return Err(MappingAnswerError::TooShort(lifetime));
  }
  Ok(GrantedMapping {
    external_port,
    maybe_external_ip,
    lifetime: lifetime.min(MAX_LIFETIME),
  })
}

fn lifetime_secs(lifetime: Duration) -> u32 {
  u32::try_from(lifetime.as_secs()).unwrap_or(u32::MAX)
}

fn be_u16(bytes: &[u8]) -> u16 {
  u16::from_be_bytes([bytes[0], bytes[1]])
}

fn be_u32(bytes: &[u8]) -> u32 {
  u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

#[cfg(test)]
mod tests {
  use super::*;

  const PORT: u16 = 40_000;
  const NONCE: [u8; PCP_NONCE_BYTES] = [9; PCP_NONCE_BYTES];
  const PUBLIC: Ipv4Addr = Ipv4Addr::new(198, 51, 100, 7);
  const HOUR: Duration = Duration::from_secs(3600);

  mod nat_pmp {
    use super::*;

    #[test]
    fn requests_are_as_the_rfc_lays_them_out() {
      assert_eq!(nat_pmp_external_address_request(), [0, 0]);
      assert_eq!(
        nat_pmp_map_request(PORT, HOUR),
        [0, 1, 0, 0, 0x9c, 0x40, 0x9c, 0x40, 0, 0, 0x0e, 0x10]
      );
      // A removal suggests no external port and asks for no time.
      assert_eq!(
        nat_pmp_map_request(PORT, Duration::ZERO),
        [0, 1, 0, 0, 0x9c, 0x40, 0, 0, 0, 0, 0, 0]
      );
    }

    #[test]
    fn a_granted_mapping_is_read() {
      let answer = map_answer(0, PORT, 41_000, 3600);
      assert_eq!(
        read_nat_pmp_map(&answer, PORT),
        Ok(GrantedMapping {
          external_port: 41_000,
          maybe_external_ip: None,
          lifetime: HOUR,
        })
      );
      assert_eq!(
        read_nat_pmp_external_address(&external_answer(PUBLIC)),
        Ok(PUBLIC)
      );
    }

    #[test]
    fn hostile_or_wrong_answers_are_refused() {
      assert_eq!(read_nat_pmp_map(&[], PORT), Err(MappingAnswerError::Length(0)));
      assert_eq!(read_nat_pmp_map(&[0], PORT), Err(MappingAnswerError::Length(1)));
      let good = map_answer(0, PORT, 41_000, 3600);
      assert_eq!(
        read_nat_pmp_map(&good[..15], PORT),
        Err(MappingAnswerError::Length(15))
      );
      let mut long = good.to_vec();
      long.push(0);
      assert_eq!(read_nat_pmp_map(&long, PORT), Err(MappingAnswerError::Length(17)));
      let mut version = good;
      version[0] = 2;
      assert_eq!(
        read_nat_pmp_map(&version, PORT),
        Err(MappingAnswerError::Version(2))
      );
      let mut request_echo = good;
      request_echo[1] = 1;
      assert_eq!(
        read_nat_pmp_map(&request_echo, PORT),
        Err(MappingAnswerError::Opcode(1))
      );
      assert_eq!(
        read_nat_pmp_map(&map_answer(2, PORT, 41_000, 3600), PORT),
        Err(MappingAnswerError::Refused(2))
      );
      assert_eq!(
        read_nat_pmp_map(&map_answer(0, PORT + 1, 41_000, 3600), PORT),
        Err(MappingAnswerError::OtherMapping)
      );
      assert_eq!(
        read_nat_pmp_map(&map_answer(0, PORT, 0, 3600), PORT),
        Err(MappingAnswerError::UnusableExternal)
      );
      assert_eq!(
        read_nat_pmp_map(&map_answer(0, PORT, 41_000, 5), PORT),
        Err(MappingAnswerError::TooShort(Duration::from_secs(5)))
      );
    }

    #[test]
    fn a_long_lifetime_is_clamped() {
      let granted = read_nat_pmp_map(&map_answer(0, PORT, 41_000, u32::MAX), PORT).unwrap();
      assert_eq!(granted.lifetime, MAX_LIFETIME);
    }

    #[test]
    fn an_external_address_inside_another_nat_is_useless() {
      for ip in [
        Ipv4Addr::new(10, 0, 0, 1),
        Ipv4Addr::new(192, 168, 1, 1),
        Ipv4Addr::new(100, 64, 0, 1),
        Ipv4Addr::new(100, 127, 255, 1),
        Ipv4Addr::UNSPECIFIED,
        Ipv4Addr::LOCALHOST,
        Ipv4Addr::BROADCAST,
        Ipv4Addr::new(224, 0, 0, 1),
        Ipv4Addr::new(169, 254, 0, 1),
        Ipv4Addr::new(250, 0, 0, 1),
      ] {
        assert_eq!(
          read_nat_pmp_external_address(&external_answer(ip)),
          Err(MappingAnswerError::UnusableExternal),
          "{ip}"
        );
      }
      assert!(is_reachable_external(Ipv4Addr::new(100, 128, 0, 1)));
    }

    #[test]
    fn a_pcp_servers_unsupported_version_answer_is_no_panic() {
      // A PCP-only router answers a version 0 request with a PCP header and UNSUPP_VERSION.
      let mut answer = [0u8; 24];
      answer[0] = 2;
      answer[1] = 0x80;
      answer[3] = 1;
      assert_eq!(
        read_nat_pmp_external_address(&answer),
        Err(MappingAnswerError::Version(2))
      );
    }

    fn map_answer(result: u16, internal: u16, external: u16, lifetime: u32) -> [u8; 16] {
      let mut answer = [0u8; 16];
      answer[1] = 129;
      answer[2..4].copy_from_slice(&result.to_be_bytes());
      answer[4..8].copy_from_slice(&77u32.to_be_bytes());
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

  mod pcp {
    use super::*;

    #[test]
    fn the_request_is_as_the_rfc_lays_it_out() {
      let request = pcp_map_request(Ipv4Addr::new(192, 168, 1, 20), PORT, HOUR, &NONCE);
      assert_eq!(request[..4], [2, 1, 0, 0]);
      assert_eq!(request[4..8], 3600u32.to_be_bytes());
      assert_eq!(
        request[8..24],
        Ipv4Addr::new(192, 168, 1, 20).to_ipv6_mapped().octets()
      );
      assert_eq!(request[24..36], NONCE);
      assert_eq!(request[36], 17);
      assert_eq!(request[40..42], PORT.to_be_bytes());
      assert_eq!(request[42..44], PORT.to_be_bytes());
      assert_eq!(request[44..60], Ipv4Addr::UNSPECIFIED.to_ipv6_mapped().octets());
    }

    #[test]
    fn a_granted_mapping_is_read_with_its_address() {
      assert_eq!(
        read_pcp_map(&answer(0, &NONCE, PORT, 41_000, PUBLIC, 3600), PORT, &NONCE),
        Ok(GrantedMapping {
          external_port: 41_000,
          maybe_external_ip: Some(PUBLIC),
          lifetime: HOUR,
        })
      );
      // Options after the MAP fields are skipped.
      let mut with_option = answer(0, &NONCE, PORT, 41_000, PUBLIC, 3600).to_vec();
      with_option.extend_from_slice(&[0x80, 0, 0, 0]);
      assert!(read_pcp_map(&with_option, PORT, &NONCE).is_ok());
    }

    #[test]
    fn hostile_or_wrong_answers_are_refused() {
      let good = answer(0, &NONCE, PORT, 41_000, PUBLIC, 3600);
      assert_eq!(
        read_pcp_map(&[], PORT, &NONCE),
        Err(MappingAnswerError::Length(0))
      );
      assert_eq!(
        read_pcp_map(&good[..59], PORT, &NONCE),
        Err(MappingAnswerError::Length(59))
      );
      assert_eq!(
        read_pcp_map(&vec![0u8; 1104], PORT, &NONCE),
        Err(MappingAnswerError::Length(1104))
      );
      let mut odd = good.to_vec();
      odd.push(0);
      assert_eq!(
        read_pcp_map(&odd, PORT, &NONCE),
        Err(MappingAnswerError::Length(61))
      );
      let mut version = good;
      version[0] = 0;
      assert_eq!(
        read_pcp_map(&version, PORT, &NONCE),
        Err(MappingAnswerError::Version(0))
      );
      let mut echo = good;
      echo[1] = 1;
      assert_eq!(
        read_pcp_map(&echo, PORT, &NONCE),
        Err(MappingAnswerError::Opcode(1))
      );
      assert_eq!(
        read_pcp_map(&answer(8, &NONCE, PORT, 41_000, PUBLIC, 3600), PORT, &NONCE),
        Err(MappingAnswerError::Refused(8))
      );
      assert_eq!(
        read_pcp_map(&answer(0, &[1; 12], PORT, 41_000, PUBLIC, 3600), PORT, &NONCE),
        Err(MappingAnswerError::OtherMapping)
      );
      assert_eq!(
        read_pcp_map(&answer(0, &NONCE, PORT + 1, 41_000, PUBLIC, 3600), PORT, &NONCE),
        Err(MappingAnswerError::OtherMapping)
      );
      let mut tcp = good;
      tcp[36] = 6;
      assert_eq!(
        read_pcp_map(&tcp, PORT, &NONCE),
        Err(MappingAnswerError::OtherMapping)
      );
      assert_eq!(
        read_pcp_map(
          &answer(0, &NONCE, PORT, 41_000, Ipv4Addr::new(10, 1, 1, 1), 3600),
          PORT,
          &NONCE
        ),
        Err(MappingAnswerError::UnusableExternal)
      );
      let mut ipv6 = good;
      ipv6[44..60].copy_from_slice(&"2001:db8::1".parse::<Ipv6Addr>().unwrap().octets());
      assert_eq!(
        read_pcp_map(&ipv6, PORT, &NONCE),
        Err(MappingAnswerError::UnusableExternal)
      );
      assert_eq!(
        read_pcp_map(&answer(0, &NONCE, PORT, 41_000, PUBLIC, 1), PORT, &NONCE),
        Err(MappingAnswerError::TooShort(Duration::from_secs(1)))
      );
    }

    #[test]
    fn random_bytes_never_panic() {
      let mut state = 0x2545_f491_4f6c_dd1du64;
      for len in 0..200 {
        let bytes: Vec<u8> = (0..len)
          .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.to_le_bytes()[0]
          })
          .collect();
        let _ = read_pcp_map(&bytes, PORT, &NONCE);
        let _ = read_nat_pmp_map(&bytes, PORT);
        let _ = read_nat_pmp_external_address(&bytes);
      }
    }

    fn answer(
      result: u8,
      nonce: &[u8; PCP_NONCE_BYTES],
      internal: u16,
      external: u16,
      ip: Ipv4Addr,
      lifetime: u32,
    ) -> [u8; 60] {
      let mut answer = [0u8; 60];
      answer[0] = 2;
      answer[1] = 0x81;
      answer[3] = result;
      answer[4..8].copy_from_slice(&lifetime.to_be_bytes());
      answer[24..36].copy_from_slice(nonce);
      answer[36] = 17;
      answer[40..42].copy_from_slice(&internal.to_be_bytes());
      answer[42..44].copy_from_slice(&external.to_be_bytes());
      answer[44..60].copy_from_slice(&ip.to_ipv6_mapped().octets());
      answer
    }
  }
}
